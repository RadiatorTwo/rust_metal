//! Direct3D 9Ex with the flip model (D3DSWAPEFFECT_FLIPEX), with `GUNMETAL_D3D9EX=1` (off by
//! default: RivaTuner's overlay does not show with it): Present hands the picture to the desktop
//! compositor without the copy and the queue of the old blit model, whose Present blocked 2-5 ms
//! now and then (PLAN.md). Without it, or when Direct3DCreate9Ex fails, everything is plain
//! Direct3D 9.
//!
//! What the game asked for and the flip model does not allow is done here:
//! - **Its back buffer** is a render target of ours (`Target`): the size, format and
//!   multisampling the game asked for (2x MSAA), with a depth-stencil surface of ours for its
//!   automatic one. GetBackBuffer hands it out; it is render target 0 and the depth-stencil
//!   surface after CreateDevice and Reset. The real swap chain (2 buffers, no multisampling) has
//!   the game's size; Present resolves the picture into it with the aspect ratio kept for the
//!   window (black bars around it) and calls PresentEx, and the compositor scales it to the
//!   window.
//! - **D3DPOOL_MANAGED** does not exist on a 9Ex device. A managed buffer is created in the
//!   default pool (lockable on 9Ex). A managed texture or cube texture is created in the
//!   default pool with a twin in system memory (`Twins`): the game's locks lock the twin, and
//!   each unlock copies it to the GPU (UpdateTexture). GetDesc and GetLevelDesc still say
//!   MANAGED.
//! - **Device loss** does not happen on 9Ex (TestCooperativeLevel says S_OK); Reset is still
//!   how the game changes mode.

use super::vtbl;
use crate::platform::{lock, log, mem};
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Direct3D9::*;
use windows::core::BOOL;

static ON: AtomicBool = AtomicBool::new(false);

/// The device is a 9Ex device with the flip model.
pub fn on() -> bool {
    ON.load(Ordering::Relaxed)
}

/// Direct3DCreate9Ex with `GUNMETAL_D3D9EX=1`: the IDirect3D9Ex (an IDirect3D9 to the game).
/// Off by default: the user saw RivaTuner's overlay vanish with it.
pub fn create(sdk: u32) -> Option<*mut c_void> {
    if !std::env::var("GUNMETAL_D3D9EX").is_ok_and(|v| v == "1") {
        return None;
    }
    match unsafe { Direct3DCreate9Ex(sdk) } {
        Ok(d) => {
            ON.store(true, Ordering::Relaxed);
            log!("d3d9: Direct3D 9Ex, flip model");
            Some(windows::core::Interface::into_raw(d))
        }
        Err(e) => {
            log!("d3d9: Direct3DCreate9Ex failed ({:#x}): plain Direct3D 9", e.code().0);
            None
        }
    }
}

// ---- managed resources ------------------------------------------------------------------

const MANAGED: u32 = 1;
const DEFAULT: u32 = 0;
const SYSTEMMEM: u32 = 2;

#[derive(Default)]
struct Twins {
    /// a managed texture or cube texture (host, default pool) -> its twin in system memory
    tex: HashMap<usize, usize>,
    /// host resources the game created as managed (buffers and textures)
    managed: std::collections::HashSet<usize>,
    /// a level's surface of such a texture -> (the texture, its lock key: level, or face and
    /// level for a cube; a cube texture)
    surf: HashMap<usize, (usize, u32, bool)>,
}

static TWINS: Mutex<Option<Twins>> = Mutex::new(None);

fn twins<R>(f: impl FnOnce(&mut Twins) -> R) -> R {
    f(lock(&TWINS).get_or_insert_with(Twins::default))
}

/// The pool to create a resource in: managed is the default pool on 9Ex.
pub fn pool(pool: u32) -> u32 {
    if on() && pool == MANAGED { DEFAULT } else { pool }
}

/// A managed buffer was created (`host`, in the default pool).
pub fn managed_buffer(host: *mut c_void) {
    twins(|t| t.managed.insert(host as usize));
}

/// A managed texture (`cube`: a cube texture) was created in the default pool: its twin in
/// system memory, with the same size, levels and format. Usage flags that the system memory
/// pool does not take are left off.
pub fn managed_texture(dev: *mut c_void, host: *mut c_void, cube: bool, size: (u32, u32), levels: u32, format: u32) -> windows::core::HRESULT {
    let v: &IDirect3DDevice9_Vtbl = unsafe { vtbl(dev) };
    let mut twin: *mut c_void = std::ptr::null_mut();
    let r = unsafe {
        if cube {
            (v.CreateCubeTexture)(dev, size.0, levels, 0, D3DFORMAT(format), D3DPOOL(SYSTEMMEM as i32), &mut twin, std::ptr::null_mut())
        } else {
            (v.CreateTexture)(dev, size.0, size.1, levels, 0, D3DFORMAT(format), D3DPOOL(SYSTEMMEM as i32), &mut twin, std::ptr::null_mut())
        }
    };
    if r.is_err() {
        log!("warn: d3d9ex: the system memory twin of a managed texture failed: {:#x}", r.0);
        return r;
    }
    twins(|t| {
        t.managed.insert(host as usize);
        t.tex.insert(host as usize, twin as usize);
    });
    r
}

/// The twin a lock of `host` goes to (a managed texture), if any.
pub fn twin(host: *mut c_void) -> Option<*mut c_void> {
    if !on() {
        return None;
    }
    twins(|t| t.tex.get(&(host as usize)).map(|&p| p as *mut c_void))
}

/// A level's surface of a managed texture was handed out: its locks go to the twin.
pub fn surface_of(surface: *mut c_void, texture: *mut c_void, key: u32, cube: bool) {
    if twin(texture).is_some() {
        twins(|t| t.surf.insert(surface as usize, (texture as usize, key, cube)));
    }
}

/// The managed texture, lock key and whether it is a cube, behind a level's surface.
pub fn surface_texture(surface: *mut c_void) -> Option<(*mut c_void, u32, bool)> {
    if !on() {
        return None;
    }
    twins(|t| t.surf.get(&(surface as usize)).map(|&(p, k, c)| (p as *mut c_void, k, c)))
}

/// After an unlock of a managed texture's twin: the whole twin to the GPU.
pub fn upload(texture: *mut c_void, cube: bool) {
    let (Some(twin), dev) = (twin(texture), super::device()) else { return };
    if dev.is_null() {
        return;
    }
    unsafe {
        if cube {
            let v: &IDirect3DCubeTexture9_Vtbl = vtbl(twin);
            for face in 0..6 {
                let _ = (v.AddDirtyRect)(twin, D3DCUBEMAP_FACES(face), std::ptr::null());
            }
        } else {
            let v: &IDirect3DTexture9_Vtbl = vtbl(twin);
            let _ = (v.AddDirtyRect)(twin, std::ptr::null());
        }
        let r = (vtbl::<IDirect3DDevice9_Vtbl>(dev).UpdateTexture)(dev, twin, texture);
        if r.is_err() {
            log!("warn: d3d9ex: UpdateTexture of a managed texture failed: {:#x}", r.0);
        }
    }
}

/// The host resource's last reference is gone: its twin and surfaces go too.
pub fn released(host: *mut c_void) {
    if !on() {
        return;
    }
    let twin = twins(|t| {
        t.managed.remove(&(host as usize));
        t.surf.retain(|_, v| v.0 != host as usize);
        t.tex.remove(&(host as usize))
    });
    if let Some(p) = twin {
        let p = p as *mut c_void;
        unsafe { (vtbl::<windows::core::IUnknown_Vtbl>(p).Release)(p) };
    }
}

/// `get` writes a description to the game's `desc`; then `fix_pool` (only if it succeeded).
pub fn fix_pool_after(host: *mut c_void, desc: u32, get: impl FnOnce() -> u32) -> u32 {
    let r = get();
    if r == 0 {
        fix_pool(host, desc);
    }
    r
}

/// A description the game reads (D3DSURFACE_DESC, D3DVERTEXBUFFER_DESC, D3DINDEXBUFFER_DESC:
/// the pool at byte 12) of `host`: managed, as the game created it.
pub fn fix_pool(host: *mut c_void, desc: u32) {
    if on() && desc != 0 && twins(|t| t.managed.contains(&(host as usize)) || t.surf.contains_key(&(host as usize))) {
        mem().wr32(desc + 12, MANAGED);
    }
}

// ---- the game's back buffer --------------------------------------------------------------

/// The game's back buffer and depth-stencil surface (host pointers), and a surface without
/// multisampling that Present resolves the picture into first.
#[derive(Clone, Copy)]
struct Target {
    rt: usize,
    ds: usize,
    resolve: usize,
    size: (u32, u32),
}

static TARGET: Mutex<Option<Target>> = Mutex::new(None);

/// What the game asked for that the flip model's swap chain does not have.
#[derive(Clone, Copy)]
pub struct Asked {
    size: (u32, u32),
    format: u32,
    count: u32,
    multisample: (i32, u32),
    swap_effect: i32,
    auto_depth: (i32, u32),
    flags: u32,
}

/// The present parameters for the host's swap chain: two buffers of the window's size,
/// D3DSWAPEFFECT_FLIPEX, no multisampling, no automatic depth-stencil surface (`Target` has
/// the game's). Returns what the game asked for, for `game_params` and `make_target`.
pub fn host_params(p: &mut D3DPRESENT_PARAMETERS) -> Asked {
    let asked = Asked {
        size: (p.BackBufferWidth, p.BackBufferHeight),
        format: p.BackBufferFormat.0,
        count: p.BackBufferCount,
        multisample: (p.MultiSampleType.0, p.MultiSampleQuality),
        swap_effect: p.SwapEffect.0,
        auto_depth: (p.EnableAutoDepthStencil.0, p.AutoDepthStencilFormat.0),
        flags: p.Flags,
    };
    // the game's size (the compositor scales it to the window, as the old Present did); the
    // window's when the game leaves it to D3D9. Not the window's at this moment: while the game
    // starts, its window may still be tiny, and a swap chain of that size stays blurry.
    let (w, h) = if asked.size.0 > 0 && asked.size.1 > 0 {
        asked.size
    } else {
        client_size(p.hDeviceWindow).filter(|&(w, h)| w > 0 && h > 0).unwrap_or((640, 480))
    };
    p.BackBufferWidth = w;
    p.BackBufferHeight = h;
    p.BackBufferFormat = D3DFMT_X8R8G8B8;
    p.BackBufferCount = 2;
    p.MultiSampleType = D3DMULTISAMPLE_NONE;
    p.MultiSampleQuality = 0;
    p.SwapEffect = D3DSWAPEFFECT_FLIPEX;
    p.EnableAutoDepthStencil = BOOL(0);
    // D3DPRESENTFLAG_LOCKABLE_BACKBUFFER, D3DPRESENTFLAG_DISCARD_DEPTHSTENCIL: ours
    p.Flags &= !0x3;
    asked
}

/// The present parameters as the game asked for them (written back to it), the back buffer's
/// size filled in as D3D9 does when it was 0.
pub fn game_params(p: &mut D3DPRESENT_PARAMETERS, a: &Asked) {
    let size = if a.size.0 == 0 || a.size.1 == 0 { (p.BackBufferWidth, p.BackBufferHeight) } else { a.size };
    (p.BackBufferWidth, p.BackBufferHeight) = size;
    p.BackBufferFormat = D3DFORMAT(a.format);
    p.BackBufferCount = a.count.max(1);
    (p.MultiSampleType, p.MultiSampleQuality) = (D3DMULTISAMPLE_TYPE(a.multisample.0), a.multisample.1);
    p.SwapEffect = D3DSWAPEFFECT(a.swap_effect);
    (p.EnableAutoDepthStencil, p.AutoDepthStencilFormat) = (BOOL(a.auto_depth.0), D3DFORMAT(a.auto_depth.1));
    p.Flags = a.flags;
}

fn client_size(hwnd: HWND) -> Option<(u32, u32)> {
    let hwnd = if hwnd.is_invalid() { super::host_hwnd(crate::platform::user32::main_window()) } else { hwnd };
    let mut r = windows_sys::Win32::Foundation::RECT { left: 0, top: 0, right: 0, bottom: 0 };
    if hwnd.is_invalid() || unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd.0, &mut r) } == 0 {
        return None;
    }
    Some((r.right.max(0) as u32, r.bottom.max(0) as u32))
}

/// After CreateDevice or Reset: the game's back buffer and depth-stencil surface, set as
/// render target 0 and depth-stencil surface, as `p` (the game's parameters, `game_params`)
/// asks.
pub fn make_target(dev: *mut c_void, p: &D3DPRESENT_PARAMETERS) -> windows::core::HRESULT {
    drop_target();
    let v: &IDirect3DDevice9_Vtbl = unsafe { vtbl(dev) };
    let (w, h, format) = (p.BackBufferWidth, p.BackBufferHeight, p.BackBufferFormat);
    let (ms, quality) = (p.MultiSampleType, p.MultiSampleQuality);
    let mut t = Target { rt: 0, ds: 0, resolve: 0, size: (w, h) };
    unsafe {
        let mut s: *mut c_void = std::ptr::null_mut();
        let r = (v.CreateRenderTarget)(dev, w, h, format, ms, quality, BOOL(0), &mut s, std::ptr::null_mut());
        if r.is_err() {
            log!("warn: d3d9ex: the game's back buffer {w}x{h} format {} multisample {} failed: {:#x}", format.0, ms.0, r.0);
            return r;
        }
        t.rt = s as usize;
        if ms != D3DMULTISAMPLE_NONE {
            let mut s: *mut c_void = std::ptr::null_mut();
            let r = (v.CreateRenderTarget)(dev, w, h, format, D3DMULTISAMPLE_NONE, 0, BOOL(0), &mut s, std::ptr::null_mut());
            if r.is_err() {
                log!("warn: d3d9ex: the resolve surface failed: {:#x}", r.0);
                release(t.rt);
                return r;
            }
            t.resolve = s as usize;
        }
        if p.EnableAutoDepthStencil.0 != 0 {
            let mut s: *mut c_void = std::ptr::null_mut();
            let discard = BOOL((p.Flags & 0x2 != 0) as i32);
            let r = (v.CreateDepthStencilSurface)(dev, w, h, p.AutoDepthStencilFormat, ms, quality, discard, &mut s, std::ptr::null_mut());
            if r.is_err() {
                log!("warn: d3d9ex: the game's depth-stencil surface failed: {:#x}", r.0);
                release(t.rt);
                release(t.resolve);
                return r;
            }
            t.ds = s as usize;
        }
        let _ = (v.SetRenderTarget)(dev, 0, t.rt as *mut c_void);
        let _ = (v.SetDepthStencilSurface)(dev, t.ds as *mut c_void);
        // the swap chain's buffers wait for at most one picture in the queue
        let ex: &IDirect3DDevice9Ex_Vtbl = vtbl(dev);
        let _ = (ex.SetMaximumFrameLatency)(dev, 1);
    }
    *lock(&TARGET) = Some(t);
    log!("d3d9ex: back buffer {w}x{h} format {} multisample {}", format.0, ms.0);
    windows::core::HRESULT(0)
}

fn release(p: usize) {
    if p != 0 {
        let p = p as *mut c_void;
        unsafe { (vtbl::<windows::core::IUnknown_Vtbl>(p).Release)(p) };
    }
}

/// Before Reset or when the device goes: our surfaces go.
pub fn drop_target() {
    if let Some(t) = lock(&TARGET).take() {
        release(t.rt);
        release(t.ds);
        release(t.resolve);
    }
}

/// The game's back buffer (GetBackBuffer), with a reference for the game.
pub fn back_buffer() -> Option<*mut c_void> {
    let t = (*lock(&TARGET))?;
    let p = t.rt as *mut c_void;
    unsafe { (vtbl::<windows::core::IUnknown_Vtbl>(p).AddRef)(p) };
    Some(p)
}

/// The game's back buffer (no reference), for pictures of it (`shot`) and its clear.
pub fn target() -> Option<*mut c_void> {
    lock(&TARGET).map(|t| t.rt as *mut c_void)
}

/// Present: the game's picture into the swap chain's buffer with its aspect ratio kept in the
/// window, black around it, then PresentEx.
pub fn present(dev: *mut c_void) -> windows::core::HRESULT {
    let Some(t) = *lock(&TARGET) else { return windows::core::HRESULT(D3DERR_INVALIDCALL_) };
    let v: &IDirect3DDevice9_Vtbl = unsafe { vtbl(dev) };
    unsafe {
        let mut real: *mut c_void = std::ptr::null_mut();
        let r = (v.GetBackBuffer)(dev, 0, 0, D3DBACKBUFFER_TYPE_MONO, &mut real);
        if r.is_err() {
            return r;
        }
        let mut desc = D3DSURFACE_DESC::default();
        let _ = (vtbl::<IDirect3DSurface9_Vtbl>(real).GetDesc)(real, &mut desc);
        let back = (desc.Width, desc.Height);
        let window = client_size(HWND::default()).filter(|&(w, h)| w > 0 && h > 0).unwrap_or(back);
        // the picture's place in the window, as a place in the swap chain's buffer (which the
        // compositor stretches to the window)
        let [x, y, w, h] = crate::platform::window::picture(window, t.size);
        let sx = |a: i32| (a as i64 * back.0 as i64 / window.0 as i64) as i32;
        let sy = |a: i32| (a as i64 * back.1 as i64 / window.1 as i64) as i32;
        let dest = RECT { left: sx(x), top: sy(y), right: sx(x + w), bottom: sy(y + h) };
        let _ = (v.ColorFill)(dev, real, std::ptr::null(), 0xff00_0000);
        let mut src = t.rt as *mut c_void;
        if t.resolve != 0 {
            let _ = (v.StretchRect)(dev, src, std::ptr::null(), t.resolve as *mut c_void, std::ptr::null(), D3DTEXF_NONE);
            src = t.resolve as *mut c_void;
        }
        let r = (v.StretchRect)(dev, src, std::ptr::null(), real, &dest, D3DTEXF_LINEAR);
        if r.is_err() {
            log_once("StretchRect into the swap chain", r);
        }
        (vtbl::<windows::core::IUnknown_Vtbl>(real).Release)(real);
        let ex: &IDirect3DDevice9Ex_Vtbl = vtbl(dev);
        (ex.PresentEx)(dev, std::ptr::null(), std::ptr::null(), HWND::default(), std::ptr::null(), 0)
    }
}

const D3DERR_INVALIDCALL_: i32 = 0x8876_086c_u32 as i32;

fn log_once(what: &str, r: windows::core::HRESULT) {
    static SEEN: AtomicBool = AtomicBool::new(false);
    if !SEEN.swap(true, Ordering::Relaxed) {
        log!("warn: d3d9ex: {what} failed: {:#x}", r.0);
    }
}
