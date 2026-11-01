//! Direct3D 9 on its own thread. The game's thread records the device calls that only set
//! state or draw (each a closure that owns copies of the data its guest pointers pointed at),
//! and they return D3D_OK at once; a worker thread makes them on the host's device in the
//! order they were recorded. Any other Direct3D call first waits until the worker has made
//! everything recorded so far (`flush`) and then runs on the calling thread as before: D3D9
//! allows that, as the device is created with D3DCREATE_MULTITHREADED. Present is recorded
//! too, and the game's thread is at most one picture ahead of the worker (`picture_done`).
//!
//! Commands go to the worker in batches (256, or at a flush or a picture's end), so the
//! worker's side takes a lock per batch, not per call. A batch is one buffer that holds the
//! closures themselves (`Batch`); the worker hands emptied batches back, so recording
//! allocates nothing once the buffers have grown (it was a Box per call, freed on the worker).
//!
//! On unless the registry value `D3DThread` is 0 or `GUNMETAL_D3D_THREAD=0`; always off with
//! Direct3D 9Ex (`ex`).

use crate::platform::log;
use std::collections::VecDeque;
use std::mem::{MaybeUninit, align_of, size_of};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

/// Commands handed to the worker at once.
const BATCH: usize = 256;

/// The unit of a batch's buffer: commands start on 16-byte boundaries.
#[repr(C, align(16))]
#[derive(Clone, Copy)]
struct Chunk([u8; 16]);

/// A command's header in a batch, followed by the closure itself in `chunks` chunks.
struct Header {
    /// moves the closure out and calls it
    run: unsafe fn(*mut u8),
    /// drops a closure that was never run
    drop: unsafe fn(*mut u8),
    chunks: usize,
}

const HEADER_CHUNKS: usize = size_of::<Header>().div_ceil(16);

/// Recorded calls, closures stored one after another in one buffer.
pub struct Batch {
    buf: Vec<MaybeUninit<Chunk>>,
    len: usize,
}

// SAFETY: only closures that are `Send` go in (`push`)
unsafe impl Send for Batch {}

impl Batch {
    fn new() -> Batch {
        Batch { buf: Vec::with_capacity(BATCH * 4), len: 0 }
    }

    fn push<F: FnOnce() + Send + 'static>(&mut self, f: F) {
        const { assert!(align_of::<F>() <= 16 && align_of::<Header>() <= 16) };
        unsafe fn run<F: FnOnce()>(p: *mut u8) {
            unsafe { std::ptr::read(p as *mut F)() }
        }
        unsafe fn drop<F>(p: *mut u8) {
            unsafe { std::ptr::drop_in_place(p as *mut F) }
        }
        let chunks = size_of::<F>().div_ceil(16);
        let at = self.buf.len();
        self.buf.resize(at + HEADER_CHUNKS + chunks, MaybeUninit::uninit());
        let base = self.buf.as_mut_ptr();
        // SAFETY: both fit in the chunks just added, which are 16-aligned
        unsafe {
            (base.add(at) as *mut Header).write(Header { run: run::<F>, drop: drop::<F>, chunks });
            (base.add(at + HEADER_CHUNKS) as *mut F).write(f);
        }
        self.len += 1;
    }

    /// Each command's header and closure address, in order.
    fn each(&mut self, mut f: impl FnMut(&Header, *mut u8)) {
        let base = self.buf.as_mut_ptr();
        let mut at = 0;
        while at < self.buf.len() {
            // SAFETY: `push` wrote a header here and its closure after it
            let h = unsafe { &*(base.add(at) as *const Header) };
            f(h, unsafe { base.add(at + HEADER_CHUNKS) } as *mut u8);
            at += HEADER_CHUNKS + h.chunks;
        }
    }

    /// Run every command in order (a panicking one is logged; the rest still run), leaving
    /// the batch empty with its buffer kept.
    fn run(&mut self) {
        self.each(|h, p| {
            // a failing call must not stop the queue: the game's thread would wait for ever
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe { (h.run)(p) })).is_err() {
                log!("d3d9 worker: a recorded call panicked");
            }
        });
        self.buf.clear();
        self.len = 0;
    }

    fn len(&self) -> usize {
        self.len
    }
}

impl Drop for Batch {
    fn drop(&mut self) {
        self.each(|h, p| unsafe { (h.drop)(p) });
    }
}

#[derive(Default)]
struct State {
    /// batches waiting for the worker, each with whether it ends a picture
    batches: VecDeque<(Batch, bool)>,
    /// emptied batches, for the recording side to reuse
    free: Vec<Batch>,
    /// commands handed to the worker, and those it has run
    submitted: u64,
    done: u64,
    /// pictures handed to the worker, and those it has finished
    pictures: u64,
    pictures_done: u64,
}

struct Shared {
    state: Mutex<State>,
    /// the worker waits here for batches
    work: Condvar,
    /// the game's thread waits here for the worker
    progress: Condvar,
}

/// How a wait for the worker is made: given the wait itself (the guest turn is handed over
/// around it, `platform::without_turn`).
pub type WaitHook = fn(&mut dyn FnMut());

pub struct Queue {
    shared: Arc<Shared>,
    wait_hook: WaitHook,
    /// commands recorded but not yet handed to the worker
    batch: Mutex<Batch>,
}

impl Queue {
    /// A queue with its worker thread ("d3d9 worker"), which runs as long as the program.
    pub fn new() -> Queue {
        Queue::with_wait_hook(|wait| wait())
    }

    /// A queue whose waits for the worker go through `hook`.
    pub fn with_wait_hook(hook: WaitHook) -> Queue {
        let shared = Arc::new(Shared { state: Mutex::new(State::default()), work: Condvar::new(), progress: Condvar::new() });
        let s = shared.clone();
        std::thread::Builder::new().name("d3d9 worker".into()).spawn(move || worker(&s)).expect("d3d9 worker thread");
        Queue { shared, wait_hook: hook, batch: Mutex::new(Batch::new()) }
    }

    /// Record a call; the worker makes it after every call recorded before.
    pub fn record<F: FnOnce() + Send + 'static>(&self, f: F) {
        let mut b = self.batch.lock().unwrap_or_else(|e| e.into_inner());
        b.push(f);
        if b.len() >= BATCH {
            self.submit(&mut b, false);
        }
    }

    /// Hand the recorded commands to the worker (`batch` gets an empty one); returns the
    /// numbers of commands and pictures handed over in all.
    fn submit(&self, batch: &mut Batch, picture: bool) -> (u64, u64) {
        let mut s = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if batch.len() == 0 && !picture {
            return (s.submitted, s.pictures);
        }
        let empty = s.free.pop().unwrap_or_else(Batch::new);
        let full = std::mem::replace(batch, empty);
        s.submitted += full.len() as u64;
        if picture {
            s.pictures += 1;
        }
        s.batches.push_back((full, picture));
        let r = (s.submitted, s.pictures);
        drop(s);
        self.shared.work.notify_one();
        r
    }

    /// Hand over what is recorded (the recording side's lock held meanwhile, so the order
    /// stays).
    fn submit_recorded(&self, picture: bool) -> (u64, u64) {
        let mut b = self.batch.lock().unwrap_or_else(|e| e.into_inner());
        self.submit(&mut b, picture)
    }

    /// Return once the worker has made every call recorded so far.
    pub fn flush(&self) {
        let (target, _) = self.submit_recorded(false);
        self.wait_until(|s| s.done >= target);
    }

    /// Wait until `ready` (through the wait hook, and only when not ready already).
    fn wait_until(&self, ready: impl Fn(&State) -> bool) {
        if ready(&self.shared.state.lock().unwrap_or_else(|e| e.into_inner())) {
            return;
        }
        (self.wait_hook)(&mut || {
            let mut s = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
            while !ready(&s) {
                s = self.shared.progress.wait(s).unwrap_or_else(|e| e.into_inner());
            }
        });
    }

    /// The picture's calls are recorded (its Present last): hand them over, then wait until
    /// the worker has finished the picture before this one.
    pub fn picture_done(&self) {
        let (_, pictures) = self.submit_recorded(true);
        self.wait_until(|s| s.pictures_done + 1 >= pictures);
    }

    /// Calls recorded and not yet made.
    pub fn pending(&self) -> usize {
        let queued = self.batch.lock().unwrap_or_else(|e| e.into_inner()).len();
        let s = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        queued + (s.submitted - s.done) as usize
    }
}

impl Default for Queue {
    fn default() -> Self {
        Queue::new()
    }
}

fn worker(s: &Shared) {
    loop {
        let (mut batch, picture) = {
            let mut st = s.state.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if let Some(b) = st.batches.pop_front() {
                    break b;
                }
                st = s.work.wait(st).unwrap_or_else(|e| e.into_inner());
            }
        };
        let n = batch.len() as u64;
        batch.run();
        let mut st = s.state.lock().unwrap_or_else(|e| e.into_inner());
        st.done += n;
        st.free.push(batch);
        if picture {
            st.pictures_done += 1;
        }
        drop(st);
        s.progress.notify_all();
    }
}

/// Whether the queue is used: the registry value `D3DThread` (default 1) and
/// `GUNMETAL_D3D_THREAD` (0 turns it off); never with Direct3D 9Ex.
pub fn on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        if std::env::var("GUNMETAL_D3D_THREAD").is_ok_and(|v| v == "0") {
            return false;
        }
        let reg = crate::platform::advapi32::value("", "D3DThread").and_then(|(_, d)| Some(u32::from_le_bytes(d.get(..4)?.try_into().ok()?)));
        reg != Some(0)
    }) && !super::ex::on()
}

static GLOBAL: OnceLock<Queue> = OnceLock::new();

/// The device's queue once CreateDevice has started it (None when off).
pub fn global() -> Option<&'static Queue> {
    GLOBAL.get()
}

/// Start the queue (CreateDevice, after it succeeded), once.
pub fn start() {
    if on() && GLOBAL.get().is_none() {
        GLOBAL.get_or_init(|| Queue::with_wait_hook(crate::platform::without_turn));
        log!("d3d9: device calls run on their own thread");
    }
}

/// Wait until the worker has made every recorded call (nothing when off).
pub fn flush() {
    if let Some(q) = global() {
        q.flush();
    }
}

/// The Direct3D methods that are recorded (or answered without the host, when they can be)
/// and so keep the turn: the game's thread does not hand over to other guest threads for them.
pub fn queued_method(name: &str) -> bool {
    const DEVICE: &[&str] = &[
        "SetRenderState", "SetTextureStageState", "SetSamplerState", "SetTransform", "SetTexture", "SetStreamSource", "SetIndices", "SetFVF",
        "SetVertexDeclaration", "SetVertexShader", "SetPixelShader", "SetVertexShaderConstantF", "SetVertexShaderConstantI", "SetVertexShaderConstantB",
        "SetPixelShaderConstantF", "SetPixelShaderConstantI", "SetPixelShaderConstantB", "SetViewport", "SetMaterial", "SetLight", "LightEnable",
        "SetScissorRect", "SetClipPlane", "Clear", "BeginScene", "EndScene", "DrawPrimitive", "DrawIndexedPrimitive", "DrawPrimitiveUP",
        "DrawIndexedPrimitiveUP", "Present", "SetGammaRamp", "TestCooperativeLevel", "GetRenderState", "GetTextureStageState", "GetSamplerState",
    ];
    match name.split_once("::") {
        Some(("IDirect3DDevice9", m)) => DEVICE.contains(&m),
        Some(("IDirect3DVertexBuffer9" | "IDirect3DIndexBuffer9", m)) => matches!(m, "Lock" | "Unlock"),
        Some(("IDirect3DQuery9", m)) => m == "Issue",
        _ => false,
    }
}
