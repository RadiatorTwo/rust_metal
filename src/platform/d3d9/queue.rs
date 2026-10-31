//! Direct3D 9 on its own thread. The game's thread records the device calls that only set
//! state or draw (each a closure that owns copies of the data its guest pointers pointed at),
//! and they return D3D_OK at once; a worker thread makes them on the host's device in the
//! order they were recorded. Any other Direct3D call first waits until the worker has made
//! everything recorded so far (`flush`) and then runs on the calling thread as before: D3D9
//! allows that, as the device is created with D3DCREATE_MULTITHREADED. Present is recorded
//! too, and the game's thread is at most one picture ahead of the worker (`picture_done`).
//!
//! Commands go to the worker in batches (256, or at a flush or a picture's end), so the
//! worker's side takes a lock per batch, not per call.
//!
//! On unless the registry value `D3DThread` is 0 or `GUNMETAL_D3D_THREAD=0`; always off with
//! Direct3D 9Ex (`ex`).

use crate::platform::log;
use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, OnceLock};

/// A recorded device call.
pub type Command = Box<dyn FnOnce() + Send>;

/// Commands handed to the worker at once.
const BATCH: usize = 256;

#[derive(Default)]
struct State {
    /// batches waiting for the worker, each with whether it ends a picture
    batches: VecDeque<(Vec<Command>, bool)>,
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

pub struct Queue {
    shared: Arc<Shared>,
    /// commands recorded but not yet handed to the worker
    batch: Mutex<Vec<Command>>,
}

impl Queue {
    /// A queue with its worker thread ("d3d9 worker"), which runs as long as the program.
    pub fn new() -> Queue {
        let shared = Arc::new(Shared { state: Mutex::new(State::default()), work: Condvar::new(), progress: Condvar::new() });
        let s = shared.clone();
        std::thread::Builder::new().name("d3d9 worker".into()).spawn(move || worker(&s)).expect("d3d9 worker thread");
        Queue { shared, batch: Mutex::new(Vec::with_capacity(BATCH)) }
    }

    /// Record a call; the worker makes it after every call recorded before.
    pub fn record(&self, c: Command) {
        let mut b = self.batch.lock().unwrap_or_else(|e| e.into_inner());
        b.push(c);
        if b.len() >= BATCH {
            let full = std::mem::replace(&mut *b, Vec::with_capacity(BATCH));
            self.submit(full, false);
        }
    }

    /// Hand the recorded commands to the worker; returns the number handed over in all.
    fn submit(&self, batch: Vec<Command>, picture: bool) -> (u64, u64) {
        let mut s = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if batch.is_empty() && !picture {
            return (s.submitted, s.pictures);
        }
        s.submitted += batch.len() as u64;
        if picture {
            s.pictures += 1;
        }
        s.batches.push_back((batch, picture));
        drop(s);
        self.shared.work.notify_one();
        let s = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        (s.submitted, s.pictures)
    }

    fn take_batch(&self) -> Vec<Command> {
        std::mem::replace(&mut *self.batch.lock().unwrap_or_else(|e| e.into_inner()), Vec::with_capacity(BATCH))
    }

    /// Return once the worker has made every call recorded so far.
    pub fn flush(&self) {
        let (target, _) = self.submit(self.take_batch(), false);
        let mut s = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        while s.done < target {
            s = self.shared.progress.wait(s).unwrap_or_else(|e| e.into_inner());
        }
    }

    /// The picture's calls are recorded (its Present last): hand them over, then wait until
    /// the worker has finished the picture before this one.
    pub fn picture_done(&self) {
        let (_, pictures) = self.submit(self.take_batch(), true);
        let mut s = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        while s.pictures_done + 1 < pictures {
            s = self.shared.progress.wait(s).unwrap_or_else(|e| e.into_inner());
        }
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
        let (batch, picture) = {
            let mut st = s.state.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if let Some(b) = st.batches.pop_front() {
                    break b;
                }
                st = s.work.wait(st).unwrap_or_else(|e| e.into_inner());
            }
        };
        let n = batch.len() as u64;
        for c in batch {
            // a failing call must not stop the queue: the game's thread would wait for ever
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(c)).is_err() {
                log!("d3d9 worker: a recorded call panicked");
            }
        }
        let mut st = s.state.lock().unwrap_or_else(|e| e.into_inner());
        st.done += n;
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
        GLOBAL.get_or_init(Queue::new);
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
