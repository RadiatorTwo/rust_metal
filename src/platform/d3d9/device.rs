//! IDirect3DDevice9: the game's calls go to the system's device. All 119 methods are in the
//! guest vtable in the SDK's order; those that hold only values or structures of the same
//! layout pass straight through, the present parameters are converted (`types`), and every
//! interface the device creates or hands out is wrapped (`resources`): textures, cube
//! textures, surfaces, buffers, queries, vertex declarations, shaders, state blocks. *UP draws
//! give D3D9 the game's own arrays (guest memory is host memory). What is not implemented
//! (swap chains, volume textures, patches, GetNPatchMode, which returns a float) returns 0 and
//! is logged at its first call.
//!
//! `GUNMETAL_SHOT=file.png`: the back buffer is written there before Present (see `shot`).
//!
//! Device loss: the host's device is always windowed, so minimising or Alt+Tab do not lose
//! it (seen); the lock screen, a secure desktop (UAC, Ctrl+Alt+Del), another program's
//! exclusive fullscreen or a display mode change can. The game asks TestCooperativeLevel
//! every frame (frame_begin 0x46f590). At D3DERR_DEVICELOST it releases its default-pool
//! resources (d3d_lost_release 0x46f190: the event query and the dynamic buffers, whose guest
//! objects go with their last Release), unacquires the keyboard and mouse, sets its lost flag
//! 0x76d51d and then waits for a message in its pump (msg_pump 0x57ac90); at
//! D3DERR_DEVICENOTRESET it calls Reset and re-creates them (d3d_restore 0x46f1f0); a failed
//! Reset is an assert. Windows repaints the windows when the device can come back; here
//! `lost_ended`, asked while the game waits in GetMessageA, gives it that WM_PAINT. The
//! answers are logged when they change ("d3d9: TestCooperativeLevel -> ..."), Reset's result
//! always. `GUNMETAL_INPUT` `lose*ms` simulates a loss (`simulate_loss`).

use super::{add_ref, cp, gp, hr, query_interface, release, resources, vtbl};
use crate::platform::{Import, arg, com, log, mem, method_not_implemented as NI, window};
use crate::rt::Cpu;
use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, Ordering};
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct3D9::*;
use windows::core::{BOOL, Interface};

/// The behaviour flags the game passed to CreateDevice (GetCreationParameters reports these,
/// not ours with D3DCREATE_MULTITHREADED).
pub(super) static GAME_FLAGS: AtomicU32 = AtomicU32::new(0);

fn dv(c: &Cpu) -> (*mut c_void, &'static IDirect3DDevice9_Vtbl) {
    let p = com::host(arg(c, 0));
    (p, unsafe { vtbl(p) })
}

/// A method whose arguments after `this` are given as host values: `pass!(name, Method, |c|
/// (args...))`; returns the HRESULT.
macro_rules! pass {
    ($f:ident, $m:ident, |$c:ident| ($($a:expr),*)) => {
        fn $f($c: &mut Cpu) -> u32 {
            let (p, v) = dv($c);
            hr(unsafe { (v.$m)(p, $($a),*) })
        }
    };
}

/// A method recorded for the worker thread when the queue is on (`queue`), else called at
/// once: `queued!(name, Method, |c| { captures } (call args))`. The captures are taken when
/// the game calls (guest data copied with `copy_in`, guest objects resolved with `host_of`);
/// the call's arguments are built from them on the worker. Returns D3D_OK when recorded; the
/// worker logs a failure once per method.
macro_rules! queued {
    ($f:ident, $m:ident, |$c:ident| { $($n:ident = $a:expr),* } ($($b:expr),*)) => {
        fn $f($c: &mut Cpu) -> u32 {
            let (p, v) = dv($c);
            let (p, f) = (p as usize, v.$m);
            $(let $n = $a;)*
            let call = move || unsafe { f(p as *mut c_void, $($b),*) };
            match super::queue::global() {
                None => hr(call()),
                Some(q) => {
                    q.record(Box::new(move || {
                        let r = call();
                        if r.is_err() {
                            worker_failed(stringify!($m), r);
                        }
                    }));
                    0
                }
            }
        }
    };
}

/// A copy of guest bytes [a, a + n) for a recorded call (None for a null pointer).
pub fn copy_in(a: u32, n: usize) -> Option<Box<[u8]>> {
    (a != 0).then(|| mem().slice(a, n).into())
}

/// The pointer a recorded call passes for a copy (null for none).
fn ptr<T>(b: &Option<Box<[u8]>>) -> *const T {
    b.as_ref().map_or(std::ptr::null(), |b| b.as_ptr() as *const T)
}

/// A recorded call failed on the worker: logged once per method.
fn worker_failed(what: &'static str, r: windows::core::HRESULT) {
    static SEEN: std::sync::Mutex<Vec<&'static str>> = std::sync::Mutex::new(Vec::new());
    let mut seen = crate::platform::lock(&SEEN);
    if !seen.contains(&what) {
        seen.push(what);
        log!("d3d9 worker: {what} -> {:#x}", r.0);
    }
}

/// Release: at 0 the device is gone (`d3d9::device` null).
fn device_release(c: &mut Cpu) -> u32 {
    let host = com::host(arg(c, 0));
    let n = release(c);
    if n == 0 {
        super::device_released(host);
    }
    n
}

fn qi(c: &mut Cpu) -> u32 {
    query_interface(c, IDirect3DDevice9::IID, "IDirect3DDevice9")
}

fn get_available_texture_mem(c: &mut Cpu) -> u32 {
    let (p, v) = dv(c);
    unsafe { (v.GetAvailableTextureMem)(p) }
}

fn get_number_of_swap_chains(c: &mut Cpu) -> u32 {
    let (p, v) = dv(c);
    unsafe { (v.GetNumberOfSwapChains)(p) }
}

fn get_direct3d(c: &mut Cpu) -> u32 {
    let (p, v) = dv(c);
    let mut d: *mut c_void = std::ptr::null_mut();
    let r = unsafe { (v.GetDirect3D)(p, &mut d) };
    mem().wr32(arg(c, 1), if r.is_ok() { com::wrap(super::IDIRECT3D9, d) } else { 0 });
    hr(r)
}

fn get_creation_parameters(c: &mut Cpu) -> u32 {
    let (p, v) = dv(c);
    let mut cp = D3DDEVICE_CREATION_PARAMETERS::default();
    let r = unsafe { (v.GetCreationParameters)(p, &mut cp) };
    if r.is_ok() {
        let a = arg(c, 1);
        let m = mem();
        m.wr32(a, cp.AdapterOrdinal);
        m.wr32(a + 4, cp.DeviceType.0 as u32);
        m.wr32(a + 8, window::guest_hwnd(cp.hFocusWindow.0 as isize));
        m.wr32(a + 12, GAME_FLAGS.load(Ordering::Relaxed));
    }
    hr(r)
}

fn set_cursor_position(c: &mut Cpu) -> u32 {
    let (p, v) = dv(c);
    unsafe { (v.SetCursorPosition)(p, arg(c, 1) as i32, arg(c, 2) as i32, arg(c, 3)) };
    0
}

fn show_cursor(c: &mut Cpu) -> u32 {
    let (p, v) = dv(c);
    unsafe { (v.ShowCursor)(p, BOOL(arg(c, 1) as i32)) }.0 as u32
}

/// Reset(present parameters): a new mode or windowed/fullscreen, as the game asks, or the
/// device back after a loss. A simulated loss (`simulate_loss`) ends here.
fn reset(c: &mut Cpu) -> u32 {
    let (p, v) = dv(c);
    let r = hr(super::with_present_params(arg(c, 1), super::device_no_16bit_modes(), "Reset", |params| unsafe {
        if !super::ex::on() {
            return (v.Reset)(p, params);
        }
        super::ex::drop_target();
        let asked = super::ex::host_params(params);
        let r = (v.Reset)(p, params);
        super::ex::game_params(params, &asked);
        if r.is_err() { r } else { super::ex::make_target(p, params) }
    }));
    log!("d3d9: Reset -> {r:#x}");
    invalidate_states();
    if r == 0 {
        *crate::platform::lock(&SIMULATED_LOSS) = None;
        LAST_COOP.store(0, Ordering::Relaxed);
    }
    r
}

/// The game's last TestCooperativeLevel said D3DERR_DEVICELOST: it waits for a message
/// (msg_pump 0x57ac90) before it asks again (`lost_ended`).
static SEEN_LOST: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// While the game waits for a message after a device loss: true once the device is no longer
/// lost (asked at most every 100 ms). Windows repaints every window when the desktop comes
/// back (after the lock screen, a secure desktop, a display mode change); `user32` then
/// gives the game the WM_PAINT that wakes it, and its next frame resets the device. Without
/// it, a loss that leaves our window's focus alone would keep the game waiting.
pub fn lost_ended() -> bool {
    static ASKED: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);
    if !SEEN_LOST.load(Ordering::Relaxed) || super::device().is_null() {
        return false;
    }
    let now = std::time::Instant::now();
    {
        let mut asked = crate::platform::lock(&ASKED);
        if asked.is_some_and(|t| now.duration_since(t) < std::time::Duration::from_millis(100)) {
            return false;
        }
        *asked = Some(now);
    }
    super::queue::flush();
    let p = super::device();
    let host = hr(unsafe { (vtbl::<IDirect3DDevice9_Vtbl>(p).TestCooperativeLevel)(p) });
    LAST_COOP.store(host, Ordering::Relaxed);
    let ended = cooperative_level(host, *crate::platform::lock(&SIMULATED_LOSS), now) != D3DERR_DEVICELOST;
    if ended {
        SEEN_LOST.store(false, Ordering::Relaxed);
        log!("d3d9: the device is back: the game's window is repainted");
    }
    ended
}

/// The host's TestCooperativeLevel after the worker's last Present (`queue`), or after the
/// game's own last question once the device was lost.
static LAST_COOP: AtomicU32 = AtomicU32::new(0);

/// TestCooperativeLevel with the worker: a fine device's answer is the worker's after its last
/// Present (`last`, no host call); once that is not fine, the host is asked (`host`), as the game
/// presents no more until it sees the device come back.
pub fn queued_coop(last: u32, host: impl FnOnce() -> u32) -> u32 {
    if last == 0 { 0 } else { host() }
}

/// A new device is fine (CreateDevice).
pub(super) fn device_created() {
    LAST_COOP.store(0, Ordering::Relaxed);
}

pub const D3DERR_DEVICELOST: u32 = 0x8876_0868;
pub const D3DERR_DEVICENOTRESET: u32 = 0x8876_0869;

/// A simulated device loss (scripts, `simulate_loss`): lost until this time, then waiting for
/// a Reset.
static SIMULATED_LOSS: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);

/// Device loss for a test: TestCooperativeLevel answers D3DERR_DEVICELOST for `d`, then
/// D3DERR_DEVICENOTRESET until the game's Reset succeeds, while the host's device stays
/// fine. The game takes its whole device-lost path (frame_begin 0x46f590: d3d_lost_release,
/// the wait in its message pump, Reset, d3d_restore), and the host's Reset checks that every
/// default-pool resource was released (D3DERR_INVALIDCALL otherwise), as after a real loss.
pub fn simulate_loss(d: std::time::Duration) {
    log!("d3d9: device loss simulated for {} ms", d.as_millis());
    *crate::platform::lock(&SIMULATED_LOSS) = Some(std::time::Instant::now() + d);
}

/// TestCooperativeLevel's answer: the host's, unless a simulated loss (lost until `until`,
/// then not reset) says otherwise.
pub fn cooperative_level(host: u32, until: Option<std::time::Instant>, now: std::time::Instant) -> u32 {
    match until {
        Some(t) if now < t => D3DERR_DEVICELOST,
        Some(_) => D3DERR_DEVICENOTRESET,
        None => host,
    }
}

/// TestCooperativeLevel, called by the game every frame (frame_begin 0x46f590). A change of
/// its answer is logged: the device lost, ready for a Reset, or fine.
fn test_cooperative_level(c: &mut Cpu) -> u32 {
    static LAST: AtomicU32 = AtomicU32::new(0);
    let (p, v) = dv(c);
    // with the worker: its answer after the last Present it made
    let host = if super::queue::global().is_some() {
        let r = queued_coop(LAST_COOP.load(Ordering::Relaxed), || {
            super::queue::flush();
            hr(unsafe { (v.TestCooperativeLevel)(p) })
        });
        LAST_COOP.store(r, Ordering::Relaxed);
        r
    } else {
        hr(unsafe { (v.TestCooperativeLevel)(p) })
    };
    let r = cooperative_level(host, *crate::platform::lock(&SIMULATED_LOSS), std::time::Instant::now());
    SEEN_LOST.store(r == D3DERR_DEVICELOST, Ordering::Relaxed);
    if LAST.swap(r, Ordering::Relaxed) != r {
        let what = match r {
            0 => "fine",
            D3DERR_DEVICELOST => "lost",
            D3DERR_DEVICENOTRESET => "lost, ready for a Reset",
            _ => "?",
        };
        log!("d3d9: TestCooperativeLevel -> {r:#x} (the device is {what})");
    }
    r
}

/// Present(source rect, dest rect, window, dirty region): the back buffer, written to
/// GUNMETAL_SHOT first when one is due. Without a rectangle of the game's, the picture keeps
/// its aspect ratio in our window (`letterbox`).
fn present(c: &mut Cpu) -> u32 {
    if let Some(q) = super::queue::global() {
        return present_queued(c, q);
    }
    let (p, v) = dv(c);
    let frames = crate::platform::frames::on();
    if frames {
        crate::platform::frames::before_present();
    }
    super::shot::before_present(p, super::ex::target());
    if super::ex::on() {
        let r = super::ex::present(p);
        if frames {
            crate::platform::frames::after_present();
        }
        if r.is_ok() {
            let t = frames.then(std::time::Instant::now);
            clear_back_buffer(p, v);
            if let Some(t) = t {
                crate::platform::frames::cleared(t.elapsed());
            }
        }
        return hr(r);
    }
    let w = arg(c, 3);
    let win = if w == 0 { HWND::default() } else { super::host_hwnd(w) };
    let target = if w == 0 { super::host_hwnd(crate::platform::user32::main_window()) } else { win };
    let dest = if arg(c, 2) == 0 { super::letterbox(target) } else { None };
    let dest_ptr = dest.as_ref().map_or(cp(arg(c, 2)), |r| r as *const _);
    let r = unsafe { (v.Present)(p, cp(arg(c, 1)), dest_ptr, win, cp(arg(c, 4))) };
    if frames {
        crate::platform::frames::after_present();
    }
    if r.is_ok() {
        let t = frames.then(std::time::Instant::now);
        clear_back_buffer(p, v);
        if let Some(t) = t {
            crate::platform::frames::cleared(t.elapsed());
        }
    }
    hr(r)
}

/// Present with the worker: recorded with copies of its rectangles and region; the worker
/// writes GUNMETAL_SHOT, presents, clears and asks TestCooperativeLevel for the game's next
/// frame. The game's thread then waits only until the picture before this one is finished.
fn present_queued(c: &Cpu, q: &'static super::queue::Queue) -> u32 {
    let frames = crate::platform::frames::on();
    if frames {
        crate::platform::frames::before_present();
    }
    let (p, v) = dv(c);
    let (src, dst, w) = (copy_in(arg(c, 1), 16), copy_in(arg(c, 2), 16), arg(c, 3));
    // RGNDATA: a 32-byte header whose nCount (at 8) rectangles of 16 bytes follow
    let region = (arg(c, 4) != 0).then(|| 32 + 16 * mem().rd32(arg(c, 4) + 8) as usize).and_then(|n| copy_in(arg(c, 4), n));
    let win = if w == 0 { 0 } else { super::host_hwnd(w).0 as usize };
    let target = if w == 0 { super::host_hwnd(crate::platform::user32::main_window()).0 as usize } else { win };
    let (p, present_fn) = (p as usize, v.Present);
    q.record(Box::new(move || {
        let p = p as *mut c_void;
        let v = unsafe { vtbl::<IDirect3DDevice9_Vtbl>(p) };
        super::shot::before_present(p, None);
        let dest = if dst.is_none() { super::letterbox(HWND(target as *mut c_void)) } else { None };
        let dest_ptr = dest.as_ref().map_or(ptr(&dst), |r| r as *const _);
        let r = unsafe { present_fn(p, ptr(&src), dest_ptr, HWND(win as *mut c_void), ptr(&region)) };
        if r.is_ok() {
            clear_back_buffer(p, v);
        }
        LAST_COOP.store(hr(unsafe { (v.TestCooperativeLevel)(p) }), Ordering::Relaxed);
    }));
    q.picture_done();
    if frames {
        crate::platform::frames::after_present();
    }
    0
}

/// The whole back buffer black after a Present. The game clears only depth and stencil and
/// draws over every pixel, except where its picture is narrower than the back buffer (its 4:3
/// menus on a wider one, `video`): the sides would keep whatever was there.
fn clear_back_buffer(p: *mut c_void, v: &IDirect3DDevice9_Vtbl) {
    unsafe {
        let mut target: *mut c_void = std::ptr::null_mut();
        if (v.GetRenderTarget)(p, 0, &mut target as *mut _ as _).is_err() || target.is_null() {
            return;
        }
        let surface = IDirect3DSurface9::from_raw(target);
        let mut desc = D3DSURFACE_DESC::default();
        let mut back: *mut c_void = std::ptr::null_mut();
        let ok = surface.GetDesc(&mut desc).is_ok()
            && match super::ex::target() {
                Some(t) => t == target,
                None => (v.GetBackBuffer)(p, 0, 0, D3DBACKBUFFER_TYPE_MONO, &mut back as *mut _ as _).is_ok() && back == target,
            };
        if !back.is_null() {
            drop(IDirect3DSurface9::from_raw(back));
        }
        if !ok {
            return;
        }
        let mut kept = D3DVIEWPORT9::default();
        if (v.GetViewport)(p, &mut kept).is_err() {
            return;
        }
        let full = D3DVIEWPORT9 { X: 0, Y: 0, Width: desc.Width, Height: desc.Height, MinZ: 0.0, MaxZ: 1.0 };
        let _ = (v.SetViewport)(p, &full);
        let _ = (v.Clear)(p, 0, std::ptr::null(), D3DCLEAR_TARGET as u32, 0xff00_0000, 1.0, 0);
        let _ = (v.SetViewport)(p, &kept);
    }
}

/// SetGammaRamp(swap chain, flags, ramp): with the worker recorded, and a ramp the same as
/// the last one is left out (the game sets it every frame in places; the host's call is slow).
fn set_gamma_ramp(c: &mut Cpu) -> u32 {
    let (p, v) = dv(c);
    let Some(q) = super::queue::global() else {
        unsafe { (v.SetGammaRamp)(p, arg(c, 1), arg(c, 2), cp(arg(c, 3))) };
        return 0;
    };
    // D3DGAMMARAMP: three arrays of 256 WORDs
    let (sc, flags, ramp) = (arg(c, 1), arg(c, 2), copy_in(arg(c, 3), 1536));
    static LAST: std::sync::Mutex<Option<(u32, u32, Box<[u8]>)>> = std::sync::Mutex::new(None);
    {
        let mut last = crate::platform::lock(&LAST);
        let now = ramp.as_ref().map(|r| (sc, flags, r.clone()));
        if now.is_some() && *last == now {
            return 0;
        }
        *last = now;
    }
    let (p, f) = (p as usize, v.SetGammaRamp);
    q.record(Box::new(move || unsafe { f(p as *mut c_void, sc, flags, ptr(&ramp)) }));
    0
}

fn get_gamma_ramp(c: &mut Cpu) -> u32 {
    let (p, v) = dv(c);
    unsafe { (v.GetGammaRamp)(p, arg(c, 1), gp(arg(c, 2))) };
    0
}

fn get_software_vertex_processing(c: &mut Cpu) -> u32 {
    let (p, v) = dv(c);
    unsafe { (v.GetSoftwareVertexProcessing)(p) }.0 as u32
}

pass!(evict_managed_resources, EvictManagedResources, |c| ());
pass!(get_device_caps, GetDeviceCaps, |c| (gp(arg(c, 1))));
pass!(get_display_mode, GetDisplayMode, |c| (arg(c, 1), gp(arg(c, 2))));
pass!(get_raster_status, GetRasterStatus, |c| (arg(c, 1), gp(arg(c, 2))));
pass!(set_dialog_box_mode, SetDialogBoxMode, |c| (BOOL(arg(c, 1) as i32)));
queued!(begin_scene, BeginScene, |c| {} ());
queued!(end_scene, EndScene, |c| {} ());
// Clear(count, rects, flags, colour, z, stencil): z is a float
queued!(clear, Clear, |c| { n = arg(c, 1), rects = copy_in(arg(c, 2), 16 * arg(c, 1) as usize), flags = arg(c, 3), color = arg(c, 4), z = arg(c, 5), st = arg(c, 6) } (n, ptr(&rects), flags, color, f32::from_bits(z), st));
queued!(set_transform, SetTransform, |c| { t = arg(c, 1), m = copy_in(arg(c, 2), 64) } (D3DTRANSFORMSTATETYPE(t as i32), ptr(&m)));
pass!(get_transform, GetTransform, |c| (D3DTRANSFORMSTATETYPE(arg(c, 1) as i32), gp(arg(c, 2))));
pass!(multiply_transform, MultiplyTransform, |c| (D3DTRANSFORMSTATETYPE(arg(c, 1) as i32), cp(arg(c, 2))));
queued!(set_viewport, SetViewport, |c| { vp = copy_in(arg(c, 1), 24) } (ptr(&vp)));
pass!(get_viewport, GetViewport, |c| (gp(arg(c, 1))));
queued!(set_material, SetMaterial, |c| { m = copy_in(arg(c, 1), 68) } (ptr(&m)));
pass!(get_material, GetMaterial, |c| (gp(arg(c, 1))));
queued!(set_light, SetLight, |c| { i = arg(c, 1), l = copy_in(arg(c, 2), 104) } (i, ptr(&l)));
pass!(get_light, GetLight, |c| (arg(c, 1), gp(arg(c, 2))));
queued!(light_enable, LightEnable, |c| { i = arg(c, 1), on = arg(c, 2) } (i, BOOL(on as i32)));
pass!(get_light_enable, GetLightEnable, |c| (arg(c, 1), gp(arg(c, 2))));
queued!(set_clip_plane, SetClipPlane, |c| { i = arg(c, 1), pl = copy_in(arg(c, 2), 16) } (i, ptr(&pl)));
pass!(get_clip_plane, GetClipPlane, |c| (arg(c, 1), gp(arg(c, 2))));
queued!(set_render_state_host, SetRenderState, |c| { k = arg(c, 1), x = arg(c, 2) } (D3DRENDERSTATETYPE(k as i32), x));
pass!(set_clip_status, SetClipStatus, |c| (cp(arg(c, 1))));
pass!(get_clip_status, GetClipStatus, |c| (gp(arg(c, 1))));
queued!(set_texture_stage_state_host, SetTextureStageState, |c| { st = arg(c, 1), k = arg(c, 2), x = arg(c, 3) } (st, D3DTEXTURESTAGESTATETYPE(k as i32), x));
queued!(set_sampler_state_host, SetSamplerState, |c| { s = arg(c, 1), k = arg(c, 2), x = arg(c, 3) } (s, D3DSAMPLERSTATETYPE(k as i32), x));
pass!(validate_device, ValidateDevice, |c| (gp(arg(c, 1))));
queued!(set_scissor_rect, SetScissorRect, |c| { r = copy_in(arg(c, 1), 16) } (ptr(&r)));
pass!(get_scissor_rect, GetScissorRect, |c| (gp(arg(c, 1))));
pass!(set_software_vertex_processing, SetSoftwareVertexProcessing, |c| (BOOL(arg(c, 1) as i32)));
pass!(set_npatch_mode, SetNPatchMode, |c| (f32::from_bits(arg(c, 1))));
queued!(set_fvf, SetFVF, |c| { fvf = arg(c, 1) } (fvf));
pass!(get_fvf, GetFVF, |c| (gp(arg(c, 1))));

// ---- states, answered from `state` while the calls are recorded ----

fn set_render_state(c: &mut Cpu) -> u32 {
    with_states(|s| s.set_render(arg(c, 1), arg(c, 2)));
    set_render_state_host(c)
}

fn set_texture_stage_state(c: &mut Cpu) -> u32 {
    with_states(|s| s.set_stage(arg(c, 1), arg(c, 2), arg(c, 3)));
    set_texture_stage_state_host(c)
}

fn set_sampler_state(c: &mut Cpu) -> u32 {
    with_states(|s| s.set_sampler(arg(c, 1), arg(c, 2), arg(c, 3)));
    set_sampler_state_host(c)
}

fn with_states<R>(f: impl FnOnce(&mut super::state::States) -> R) -> R {
    f(crate::platform::lock(&super::state::STATES).get_or_insert_with(Default::default))
}

/// The host's states changed behind the cache (Reset, state blocks): the next Get refills it.
pub(super) fn invalidate_states() {
    with_states(|s| s.invalidate());
}

/// A Get*State: from the cache while the queue is on; otherwise, or when the cache does not
/// hold it, the host's after a flush, which also fills the whole cache if it is stale.
fn get_state(c: &Cpu, out: u32, cached: impl Fn(&super::state::States) -> Option<u32>, host: impl FnOnce(*mut c_void, &'static IDirect3DDevice9_Vtbl) -> windows::core::HRESULT) -> u32 {
    let on = super::queue::global().is_some();
    if on && let Some(x) = with_states(|s| cached(s)) {
        mem().wr32(out, x);
        return 0;
    }
    super::queue::flush();
    let (p, v) = dv(c);
    if on && !with_states(|s| s.valid()) {
        fill_states(p, v);
        if let Some(x) = with_states(|s| cached(s)) {
            mem().wr32(out, x);
            return 0;
        }
    }
    hr(host(p, v))
}

/// Every render, texture stage and sampler state from the host's device into the cache.
fn fill_states(p: *mut c_void, v: &IDirect3DDevice9_Vtbl) {
    let (render, (stages, stage_states), (samplers, sampler_states)) = super::state::States::sizes();
    with_states(|s| unsafe {
        for k in 0..render {
            let mut x = 0;
            let _ = (v.GetRenderState)(p, D3DRENDERSTATETYPE(k as i32), &mut x);
            s.set_render(k, x);
        }
        for st in 0..stages {
            for k in 0..stage_states {
                let mut x = 0;
                let _ = (v.GetTextureStageState)(p, st, D3DTEXTURESTAGESTATETYPE(k as i32), &mut x);
                s.set_stage(st, k, x);
            }
        }
        for sm in 0..samplers {
            for k in 0..sampler_states {
                let mut x = 0;
                let _ = (v.GetSamplerState)(p, sm, D3DSAMPLERSTATETYPE(k as i32), &mut x);
                s.set_sampler(sm, k, x);
            }
        }
        s.validate();
    });
}

fn get_render_state(c: &mut Cpu) -> u32 {
    let (k, out) = (arg(c, 1), arg(c, 2));
    get_state(c, out, |s| s.render(k), |p, v| unsafe { (v.GetRenderState)(p, D3DRENDERSTATETYPE(k as i32), gp(out)) })
}

fn get_texture_stage_state(c: &mut Cpu) -> u32 {
    let (st, k, out) = (arg(c, 1), arg(c, 2), arg(c, 3));
    get_state(c, out, |s| s.stage(st, k), |p, v| unsafe { (v.GetTextureStageState)(p, st, D3DTEXTURESTAGESTATETYPE(k as i32), gp(out)) })
}

fn get_sampler_state(c: &mut Cpu) -> u32 {
    let (sm, k, out) = (arg(c, 1), arg(c, 2), arg(c, 3));
    get_state(c, out, |s| s.sampler(sm, k), |p, v| unsafe { (v.GetSamplerState)(p, sm, D3DSAMPLERSTATETYPE(k as i32), gp(out)) })
}

// ---- resources (`resources`) ----

fn create_texture(c: &mut Cpu) -> u32 {
    let (p, v) = dv(c);
    let a = |k| arg(c, k);
    let mut t: *mut c_void = std::ptr::null_mut();
    let pool = super::ex::pool(a(6));
    let mut r = unsafe { (v.CreateTexture)(p, a(1), a(2), a(3), a(4), D3DFORMAT(a(5)), D3DPOOL(pool as i32), &mut t, std::ptr::null_mut()) };
    if r.is_ok() && pool != a(6) {
        let twin = super::ex::managed_texture(p, t, false, (a(1), a(2)), a(3), a(5));
        r = managed_twin(&mut t, twin);
    }
    resources::created(r, t, resources::TEXTURE, a(7), "CreateTexture")
}

fn create_vertex_buffer(c: &mut Cpu) -> u32 {
    let (p, v) = dv(c);
    let a = |k| arg(c, k);
    let mut b: *mut c_void = std::ptr::null_mut();
    let pool = super::ex::pool(a(4));
    let r = unsafe { (v.CreateVertexBuffer)(p, a(1), a(2), a(3), D3DPOOL(pool as i32), &mut b, std::ptr::null_mut()) };
    if r.is_ok() && pool != a(4) {
        super::ex::managed_buffer(b);
    }
    resources::created(r, b, resources::VERTEX_BUFFER, a(5), "CreateVertexBuffer")
}

fn create_index_buffer(c: &mut Cpu) -> u32 {
    let (p, v) = dv(c);
    let a = |k| arg(c, k);
    let mut b: *mut c_void = std::ptr::null_mut();
    let pool = super::ex::pool(a(4));
    let r = unsafe { (v.CreateIndexBuffer)(p, a(1), a(2), D3DFORMAT(a(3)), D3DPOOL(pool as i32), &mut b, std::ptr::null_mut()) };
    if r.is_ok() && pool != a(4) {
        super::ex::managed_buffer(b);
    }
    resources::created(r, b, resources::INDEX_BUFFER, a(5), "CreateIndexBuffer")
}

/// CreateQuery(type, out): a null out pointer asks only whether the type is supported.
fn create_query(c: &mut Cpu) -> u32 {
    let (p, v) = dv(c);
    let (ty, out) = (arg(c, 1), arg(c, 2));
    let mut q: *mut c_void = std::ptr::null_mut();
    let r = unsafe { (v.CreateQuery)(p, D3DQUERYTYPE(ty as i32), if out == 0 { std::ptr::null_mut() } else { &mut q }) };
    resources::created(r, q, resources::QUERY, out, "CreateQuery")
}

queued!(set_texture, SetTexture, |c| { st = arg(c, 1), t = resources::host_of(arg(c, 2)) as usize } (st, t as _));
queued!(set_stream_source, SetStreamSource, |c| { s = arg(c, 1), b = resources::host_of(arg(c, 2)) as usize, off = arg(c, 3), stride = arg(c, 4) } (s, b as _, off, stride));
queued!(set_indices, SetIndices, |c| { b = resources::host_of(arg(c, 1)) as usize } (b as _));
// PALETTEENTRY: 4 bytes
pass!(set_palette_entries, SetPaletteEntries, |c| (arg(c, 1), cp(arg(c, 2))));
pass!(get_palette_entries, GetPaletteEntries, |c| (arg(c, 1), gp(arg(c, 2))));
pass!(set_current_texture_palette, SetCurrentTexturePalette, |c| (arg(c, 1)));
pass!(get_current_texture_palette, GetCurrentTexturePalette, |c| (gp(arg(c, 1))));

// SetCursorProperties(x hotspot, y hotspot, surface)
pass!(set_cursor_properties, SetCursorProperties, |c| (arg(c, 1), arg(c, 2), resources::host_of(arg(c, 3))));

/// A host interface the device hands out (`get` fills it) into the game's out pointer at
/// `out`; `wrap` finds or makes its guest object. Null out of D3D9 gives 0 and its result.
fn handed_out(c: &Cpu, out: u32, what: &str, wrap: impl FnOnce(*mut c_void) -> u32, get: impl FnOnce(*mut c_void, &'static IDirect3DDevice9_Vtbl, *mut *mut c_void) -> windows::core::HRESULT) -> u32 {
    let (p, v) = dv(c);
    let mut o: *mut c_void = std::ptr::null_mut();
    let r = get(p, v, &mut o);
    if r.is_err() || o.is_null() {
        if r.is_err() {
            log!("warn: {what} failed: {:#x}", r.0);
        }
        mem().wr32(out, 0);
        return hr(r);
    }
    mem().wr32(out, wrap(o));
    0
}

fn surface(h: *mut c_void) -> u32 {
    com::wrap(resources::SURFACE, h)
}

/// GetBackBuffer(swap chain, index, type, out).
fn get_back_buffer(c: &mut Cpu) -> u32 {
    let (sc, i, ty) = (arg(c, 1), arg(c, 2), arg(c, 3));
    handed_out(c, arg(c, 4), "GetBackBuffer", surface, |p, v, o| unsafe {
        // 9Ex: the game's back buffer is ours (`ex`)
        match super::ex::back_buffer() {
            Some(b) => {
                *o = b;
                windows::core::HRESULT(0)
            }
            None => (v.GetBackBuffer)(p, sc, i, D3DBACKBUFFER_TYPE(ty as i32), o),
        }
    })
}

/// CreateCubeTexture(edge, levels, usage, format, pool, out, shared handle).
fn create_cube_texture(c: &mut Cpu) -> u32 {
    let (p, v) = dv(c);
    let a = |k| arg(c, k);
    let mut t: *mut c_void = std::ptr::null_mut();
    let pool = super::ex::pool(a(5));
    let mut r = unsafe { (v.CreateCubeTexture)(p, a(1), a(2), a(3), D3DFORMAT(a(4)), D3DPOOL(pool as i32), &mut t, std::ptr::null_mut()) };
    if r.is_ok() && pool != a(5) {
        let twin = super::ex::managed_texture(p, t, true, (a(1), a(1)), a(2), a(4));
        r = managed_twin(&mut t, twin);
    }
    resources::created(r, t, resources::CUBE_TEXTURE, a(6), "CreateCubeTexture")
}

/// A managed texture's twin (`ex::managed_texture`) failed: the texture goes too.
fn managed_twin(t: &mut *mut c_void, r: windows::core::HRESULT) -> windows::core::HRESULT {
    if r.is_err() {
        unsafe { (super::vtbl::<windows::core::IUnknown_Vtbl>(*t).Release)(*t) };
        *t = std::ptr::null_mut();
    }
    r
}

/// CreateRenderTarget(w, h, format, multisample, quality, lockable, out, shared handle).
fn create_render_target(c: &mut Cpu) -> u32 {
    let (p, v) = dv(c);
    let a = |k| arg(c, k);
    let mut s: *mut c_void = std::ptr::null_mut();
    let r = unsafe {
        (v.CreateRenderTarget)(p, a(1), a(2), D3DFORMAT(a(3)), D3DMULTISAMPLE_TYPE(a(4) as i32), a(5), BOOL(a(6) as i32), &mut s, std::ptr::null_mut())
    };
    resources::created(r, s, resources::SURFACE, a(7), "CreateRenderTarget")
}

/// CreateDepthStencilSurface(w, h, format, multisample, quality, discard, out, shared handle).
fn create_depth_stencil_surface(c: &mut Cpu) -> u32 {
    let (p, v) = dv(c);
    let a = |k| arg(c, k);
    let mut s: *mut c_void = std::ptr::null_mut();
    let r = unsafe {
        (v.CreateDepthStencilSurface)(p, a(1), a(2), D3DFORMAT(a(3)), D3DMULTISAMPLE_TYPE(a(4) as i32), a(5), BOOL(a(6) as i32), &mut s, std::ptr::null_mut())
    };
    resources::created(r, s, resources::SURFACE, a(7), "CreateDepthStencilSurface")
}

/// CreateOffscreenPlainSurface(w, h, format, pool, out, shared handle).
fn create_offscreen_plain_surface(c: &mut Cpu) -> u32 {
    let (p, v) = dv(c);
    let a = |k| arg(c, k);
    let mut s: *mut c_void = std::ptr::null_mut();
    let r = unsafe { (v.CreateOffscreenPlainSurface)(p, a(1), a(2), D3DFORMAT(a(3)), D3DPOOL(a(4) as i32), &mut s, std::ptr::null_mut()) };
    resources::created(r, s, resources::SURFACE, a(5), "CreateOffscreenPlainSurface")
}

use resources::host_of as h;

// Copies between surfaces and textures: the destination is never a buffer, so no kept guest
// copy goes stale (textures and surfaces are copied in at every lock).
pass!(update_surface, UpdateSurface, |c| (h(arg(c, 1)), cp(arg(c, 2)), h(arg(c, 3)), cp(arg(c, 4))));
pass!(update_texture, UpdateTexture, |c| (h(arg(c, 1)), h(arg(c, 2))));
pass!(get_render_target_data, GetRenderTargetData, |c| (h(arg(c, 1)), h(arg(c, 2))));
pass!(get_front_buffer_data, GetFrontBufferData, |c| (arg(c, 1), h(arg(c, 2))));
pass!(stretch_rect, StretchRect, |c| (h(arg(c, 1)), cp(arg(c, 2)), h(arg(c, 3)), cp(arg(c, 4)), D3DTEXTUREFILTERTYPE(arg(c, 5) as i32)));
pass!(color_fill, ColorFill, |c| (h(arg(c, 1)), cp(arg(c, 2)), arg(c, 3)));
pass!(set_render_target, SetRenderTarget, |c| (arg(c, 1), h(arg(c, 2))));
pass!(set_depth_stencil_surface, SetDepthStencilSurface, |c| (h(arg(c, 1))));

fn get_render_target(c: &mut Cpu) -> u32 {
    let i = arg(c, 1);
    handed_out(c, arg(c, 2), "GetRenderTarget", surface, |p, v, o| unsafe { (v.GetRenderTarget)(p, i, o) })
}

fn get_depth_stencil_surface(c: &mut Cpu) -> u32 {
    handed_out(c, arg(c, 1), "GetDepthStencilSurface", surface, |p, v, o| unsafe { (v.GetDepthStencilSurface)(p, o) })
}

// ---- state blocks ----

fn create_state_block(c: &mut Cpu) -> u32 {
    let (p, v) = dv(c);
    let mut b: *mut c_void = std::ptr::null_mut();
    let r = unsafe { (v.CreateStateBlock)(p, D3DSTATEBLOCKTYPE(arg(c, 1) as i32), &mut b) };
    resources::created(r, b, resources::STATE_BLOCK, arg(c, 2), "CreateStateBlock")
}

// while a state block records, Set* calls do not change the device: the cache is refilled after
fn begin_state_block(c: &mut Cpu) -> u32 {
    invalidate_states();
    let (p, v) = dv(c);
    hr(unsafe { (v.BeginStateBlock)(p) })
}

fn end_state_block(c: &mut Cpu) -> u32 {
    invalidate_states();
    let (p, v) = dv(c);
    let mut b: *mut c_void = std::ptr::null_mut();
    let r = unsafe { (v.EndStateBlock)(p, &mut b) };
    resources::created(r, b, resources::STATE_BLOCK, arg(c, 1), "EndStateBlock")
}

fn get_texture(c: &mut Cpu) -> u32 {
    let stage = arg(c, 1);
    handed_out(c, arg(c, 2), "GetTexture", resources::wrap_base_texture, |p, v, o| unsafe { (v.GetTexture)(p, stage, o) })
}

// ---- drawing ----

queued!(draw_primitive, DrawPrimitive, |c| { ty = arg(c, 1), start = arg(c, 2), n = arg(c, 3) } (D3DPRIMITIVETYPE(ty as i32), start, n));
queued!(draw_indexed_primitive, DrawIndexedPrimitive, |c| { ty = arg(c, 1), base = arg(c, 2), min = arg(c, 3), nv = arg(c, 4), start = arg(c, 5), n = arg(c, 6) } (D3DPRIMITIVETYPE(ty as i32), base as i32, min, nv, start, n));

/// A *UP draw's arrays not all in guest memory: logged, D3DERR_INVALIDCALL.
fn up_refused(what: &str, a: u32, n: u32) -> bool {
    if super::super::guest_readable(a, n) {
        return false;
    }
    log!("warn: {what}: {n} bytes at {a:#x} not in guest memory (D3DERR_INVALIDCALL)");
    true
}

/// DrawPrimitiveUP(type, count, vertices, stride): D3D9 reads the vertices where the game has
/// them (guest memory is host memory).
fn draw_primitive_up(c: &mut Cpu) -> u32 {
    let (p, v) = dv(c);
    let (ty, count, data, stride) = (arg(c, 1), arg(c, 2), arg(c, 3), arg(c, 4));
    if up_refused("DrawPrimitiveUP", data, resources::primitive_vertices(ty, count).saturating_mul(stride)) {
        return resources::D3DERR_INVALIDCALL;
    }
    let n = resources::primitive_vertices(ty, count).saturating_mul(stride) as usize;
    if let Some(q) = super::queue::global() {
        let (p, f, vertices) = (p as usize, v.DrawPrimitiveUP, copy_in(data, n));
        q.record(Box::new(move || {
            let r = unsafe { f(p as *mut c_void, D3DPRIMITIVETYPE(ty as i32), count, ptr(&vertices), stride) };
            if r.is_err() {
                worker_failed("DrawPrimitiveUP", r);
            }
        }));
        return 0;
    }
    hr(unsafe { (v.DrawPrimitiveUP)(p, D3DPRIMITIVETYPE(ty as i32), count, cp(data), stride) })
}

/// DrawIndexedPrimitiveUP(type, min index, vertex count, primitive count, indices, index
/// format, vertices, stride).
fn draw_indexed_primitive_up(c: &mut Cpu) -> u32 {
    let (p, v) = dv(c);
    let a = |k| arg(c, k);
    let (ty, min, nv, count, idx, fmt, data, stride) = (a(1), a(2), a(3), a(4), a(5), a(6), a(7), a(8));
    // D3DFMT_INDEX16 101, D3DFMT_INDEX32 102
    let index_bytes = resources::primitive_vertices(ty, count).saturating_mul(if fmt == 102 { 4 } else { 2 });
    if up_refused("DrawIndexedPrimitiveUP indices", idx, index_bytes)
        || up_refused("DrawIndexedPrimitiveUP vertices", data, min.saturating_add(nv).saturating_mul(stride))
    {
        return resources::D3DERR_INVALIDCALL;
    }
    if let Some(q) = super::queue::global() {
        let vertex_bytes = min.saturating_add(nv).saturating_mul(stride) as usize;
        let (p, f, indices, vertices) = (p as usize, v.DrawIndexedPrimitiveUP, copy_in(idx, index_bytes as usize), copy_in(data, vertex_bytes));
        q.record(Box::new(move || {
            let r = unsafe { f(p as *mut c_void, D3DPRIMITIVETYPE(ty as i32), min, nv, count, ptr(&indices), D3DFORMAT(fmt), ptr(&vertices), stride) };
            if r.is_err() {
                worker_failed("DrawIndexedPrimitiveUP", r);
            }
        }));
        return 0;
    }
    hr(unsafe { (v.DrawIndexedPrimitiveUP)(p, D3DPRIMITIVETYPE(ty as i32), min, nv, count, cp(idx), D3DFORMAT(fmt), cp(data), stride) })
}

/// ProcessVertices(source start, destination index, count, destination buffer, declaration,
/// flags): the GPU writes the buffer, whose kept guest copy is stale then.
fn process_vertices(c: &mut Cpu) -> u32 {
    let (p, v) = dv(c);
    let a = |k| arg(c, k);
    resources::stale(a(4));
    hr(unsafe { (v.ProcessVertices)(p, a(1), a(2), a(3), h(a(4)), h(a(5)), a(6)) })
}

// ---- vertex declarations and shaders ----

/// CreateVertexDeclaration(elements, out): the array copied up to its end marker.
fn create_vertex_declaration(c: &mut Cpu) -> u32 {
    let (p, v) = dv(c);
    let elements = resources::vertex_elements(arg(c, 1));
    let mut d: *mut c_void = std::ptr::null_mut();
    let r = unsafe { (v.CreateVertexDeclaration)(p, elements.as_ptr(), &mut d) };
    resources::created(r, d, resources::VERTEX_DECLARATION, arg(c, 2), "CreateVertexDeclaration")
}

queued!(set_vertex_declaration, SetVertexDeclaration, |c| { d = h(arg(c, 1)) as usize } (d as _));

fn get_vertex_declaration(c: &mut Cpu) -> u32 {
    handed_out(c, arg(c, 1), "GetVertexDeclaration", |o| com::wrap(resources::VERTEX_DECLARATION, o), |p, v, o| unsafe { (v.GetVertexDeclaration)(p, o) })
}

/// CreateVertexShader(bytecode, out): the game's bytecode (from its D3DX9) as it is; D3D9
/// reads it to its end token.
fn create_vertex_shader(c: &mut Cpu) -> u32 {
    let (p, v) = dv(c);
    let mut s: *mut c_void = std::ptr::null_mut();
    let r = unsafe { (v.CreateVertexShader)(p, cp(arg(c, 1)), &mut s) };
    resources::created(r, s, resources::VERTEX_SHADER, arg(c, 2), "CreateVertexShader")
}

fn create_pixel_shader(c: &mut Cpu) -> u32 {
    let (p, v) = dv(c);
    let mut s: *mut c_void = std::ptr::null_mut();
    let r = unsafe { (v.CreatePixelShader)(p, cp(arg(c, 1)), &mut s) };
    resources::created(r, s, resources::PIXEL_SHADER, arg(c, 2), "CreatePixelShader")
}

queued!(set_vertex_shader, SetVertexShader, |c| { sh = h(arg(c, 1)) as usize } (sh as _));
queued!(set_pixel_shader, SetPixelShader, |c| { sh = h(arg(c, 1)) as usize } (sh as _));

fn get_vertex_shader(c: &mut Cpu) -> u32 {
    handed_out(c, arg(c, 1), "GetVertexShader", |o| com::wrap(resources::VERTEX_SHADER, o), |p, v, o| unsafe { (v.GetVertexShader)(p, o) })
}

fn get_pixel_shader(c: &mut Cpu) -> u32 {
    handed_out(c, arg(c, 1), "GetPixelShader", |o| com::wrap(resources::PIXEL_SHADER, o), |p, v, o| unsafe { (v.GetPixelShader)(p, o) })
}

// constants (start register, data, count): floats, ints and BOOLs of 4 bytes, the same layout
queued!(set_vs_const_f, SetVertexShaderConstantF, |c| { r = arg(c, 1), d = copy_in(arg(c, 2), 16 * arg(c, 3) as usize), n = arg(c, 3) } (r, ptr(&d), n));
pass!(get_vs_const_f, GetVertexShaderConstantF, |c| (arg(c, 1), gp(arg(c, 2)), arg(c, 3)));
queued!(set_vs_const_i, SetVertexShaderConstantI, |c| { r = arg(c, 1), d = copy_in(arg(c, 2), 16 * arg(c, 3) as usize), n = arg(c, 3) } (r, ptr(&d), n));
pass!(get_vs_const_i, GetVertexShaderConstantI, |c| (arg(c, 1), gp(arg(c, 2)), arg(c, 3)));
queued!(set_vs_const_b, SetVertexShaderConstantB, |c| { r = arg(c, 1), d = copy_in(arg(c, 2), 4 * arg(c, 3) as usize), n = arg(c, 3) } (r, ptr(&d), n));
pass!(get_vs_const_b, GetVertexShaderConstantB, |c| (arg(c, 1), gp(arg(c, 2)), arg(c, 3)));
queued!(set_ps_const_f, SetPixelShaderConstantF, |c| { r = arg(c, 1), d = copy_in(arg(c, 2), 16 * arg(c, 3) as usize), n = arg(c, 3) } (r, ptr(&d), n));
pass!(get_ps_const_f, GetPixelShaderConstantF, |c| (arg(c, 1), gp(arg(c, 2)), arg(c, 3)));
queued!(set_ps_const_i, SetPixelShaderConstantI, |c| { r = arg(c, 1), d = copy_in(arg(c, 2), 16 * arg(c, 3) as usize), n = arg(c, 3) } (r, ptr(&d), n));
pass!(get_ps_const_i, GetPixelShaderConstantI, |c| (arg(c, 1), gp(arg(c, 2)), arg(c, 3)));
queued!(set_ps_const_b, SetPixelShaderConstantB, |c| { r = arg(c, 1), d = copy_in(arg(c, 2), 4 * arg(c, 3) as usize), n = arg(c, 3) } (r, ptr(&d), n));
pass!(get_ps_const_b, GetPixelShaderConstantB, |c| (arg(c, 1), gp(arg(c, 2)), arg(c, 3)));

// ---- streams ----

/// GetStreamSource(stream, out buffer, out offset, out stride).
fn get_stream_source(c: &mut Cpu) -> u32 {
    let (s, off, stride) = (arg(c, 1), arg(c, 3), arg(c, 4));
    handed_out(c, arg(c, 2), "GetStreamSource", |o| com::wrap(resources::VERTEX_BUFFER, o), |p, v, o| unsafe { (v.GetStreamSource)(p, s, o, gp(off), gp(stride)) })
}

pass!(set_stream_source_freq, SetStreamSourceFreq, |c| (arg(c, 1), arg(c, 2)));
pass!(get_stream_source_freq, GetStreamSourceFreq, |c| (arg(c, 1), gp(arg(c, 2))));

fn get_indices(c: &mut Cpu) -> u32 {
    handed_out(c, arg(c, 1), "GetIndices", |o| com::wrap(resources::INDEX_BUFFER, o), |p, v, o| unsafe { (v.GetIndices)(p, o) })
}

/// IDirect3DDevice9, in the SDK's order (119 methods).
pub static METHODS: &[Import] = &[
    ("IDirect3DDevice9::QueryInterface", 3, qi),
    ("IDirect3DDevice9::AddRef", 1, add_ref),
    ("IDirect3DDevice9::Release", 1, device_release),
    ("IDirect3DDevice9::TestCooperativeLevel", 1, test_cooperative_level),
    ("IDirect3DDevice9::GetAvailableTextureMem", 1, get_available_texture_mem),
    ("IDirect3DDevice9::EvictManagedResources", 1, evict_managed_resources),
    ("IDirect3DDevice9::GetDirect3D", 2, get_direct3d),
    ("IDirect3DDevice9::GetDeviceCaps", 2, get_device_caps),
    ("IDirect3DDevice9::GetDisplayMode", 3, get_display_mode),
    ("IDirect3DDevice9::GetCreationParameters", 2, get_creation_parameters),
    ("IDirect3DDevice9::SetCursorProperties", 4, set_cursor_properties),
    ("IDirect3DDevice9::SetCursorPosition", 4, set_cursor_position),
    ("IDirect3DDevice9::ShowCursor", 2, show_cursor),
    ("IDirect3DDevice9::CreateAdditionalSwapChain", 3, NI),
    ("IDirect3DDevice9::GetSwapChain", 3, NI),
    ("IDirect3DDevice9::GetNumberOfSwapChains", 1, get_number_of_swap_chains),
    ("IDirect3DDevice9::Reset", 2, reset),
    ("IDirect3DDevice9::Present", 5, present),
    ("IDirect3DDevice9::GetBackBuffer", 5, get_back_buffer),
    ("IDirect3DDevice9::GetRasterStatus", 3, get_raster_status),
    ("IDirect3DDevice9::SetDialogBoxMode", 2, set_dialog_box_mode),
    ("IDirect3DDevice9::SetGammaRamp", 4, set_gamma_ramp),
    ("IDirect3DDevice9::GetGammaRamp", 3, get_gamma_ramp),
    ("IDirect3DDevice9::CreateTexture", 9, create_texture),
    ("IDirect3DDevice9::CreateVolumeTexture", 10, NI),
    ("IDirect3DDevice9::CreateCubeTexture", 8, create_cube_texture),
    ("IDirect3DDevice9::CreateVertexBuffer", 7, create_vertex_buffer),
    ("IDirect3DDevice9::CreateIndexBuffer", 7, create_index_buffer),
    ("IDirect3DDevice9::CreateRenderTarget", 9, create_render_target),
    ("IDirect3DDevice9::CreateDepthStencilSurface", 9, create_depth_stencil_surface),
    ("IDirect3DDevice9::UpdateSurface", 5, update_surface),
    ("IDirect3DDevice9::UpdateTexture", 3, update_texture),
    ("IDirect3DDevice9::GetRenderTargetData", 3, get_render_target_data),
    ("IDirect3DDevice9::GetFrontBufferData", 3, get_front_buffer_data),
    ("IDirect3DDevice9::StretchRect", 6, stretch_rect),
    ("IDirect3DDevice9::ColorFill", 4, color_fill),
    ("IDirect3DDevice9::CreateOffscreenPlainSurface", 7, create_offscreen_plain_surface),
    ("IDirect3DDevice9::SetRenderTarget", 3, set_render_target),
    ("IDirect3DDevice9::GetRenderTarget", 3, get_render_target),
    ("IDirect3DDevice9::SetDepthStencilSurface", 2, set_depth_stencil_surface),
    ("IDirect3DDevice9::GetDepthStencilSurface", 2, get_depth_stencil_surface),
    ("IDirect3DDevice9::BeginScene", 1, begin_scene),
    ("IDirect3DDevice9::EndScene", 1, end_scene),
    ("IDirect3DDevice9::Clear", 7, clear),
    ("IDirect3DDevice9::SetTransform", 3, set_transform),
    ("IDirect3DDevice9::GetTransform", 3, get_transform),
    ("IDirect3DDevice9::MultiplyTransform", 3, multiply_transform),
    ("IDirect3DDevice9::SetViewport", 2, set_viewport),
    ("IDirect3DDevice9::GetViewport", 2, get_viewport),
    ("IDirect3DDevice9::SetMaterial", 2, set_material),
    ("IDirect3DDevice9::GetMaterial", 2, get_material),
    ("IDirect3DDevice9::SetLight", 3, set_light),
    ("IDirect3DDevice9::GetLight", 3, get_light),
    ("IDirect3DDevice9::LightEnable", 3, light_enable),
    ("IDirect3DDevice9::GetLightEnable", 3, get_light_enable),
    ("IDirect3DDevice9::SetClipPlane", 3, set_clip_plane),
    ("IDirect3DDevice9::GetClipPlane", 3, get_clip_plane),
    ("IDirect3DDevice9::SetRenderState", 3, set_render_state),
    ("IDirect3DDevice9::GetRenderState", 3, get_render_state),
    ("IDirect3DDevice9::CreateStateBlock", 3, create_state_block),
    ("IDirect3DDevice9::BeginStateBlock", 1, begin_state_block),
    ("IDirect3DDevice9::EndStateBlock", 2, end_state_block),
    ("IDirect3DDevice9::SetClipStatus", 2, set_clip_status),
    ("IDirect3DDevice9::GetClipStatus", 2, get_clip_status),
    ("IDirect3DDevice9::GetTexture", 3, get_texture),
    ("IDirect3DDevice9::SetTexture", 3, set_texture),
    ("IDirect3DDevice9::GetTextureStageState", 4, get_texture_stage_state),
    ("IDirect3DDevice9::SetTextureStageState", 4, set_texture_stage_state),
    ("IDirect3DDevice9::GetSamplerState", 4, get_sampler_state),
    ("IDirect3DDevice9::SetSamplerState", 4, set_sampler_state),
    ("IDirect3DDevice9::ValidateDevice", 2, validate_device),
    ("IDirect3DDevice9::SetPaletteEntries", 3, set_palette_entries),
    ("IDirect3DDevice9::GetPaletteEntries", 3, get_palette_entries),
    ("IDirect3DDevice9::SetCurrentTexturePalette", 2, set_current_texture_palette),
    ("IDirect3DDevice9::GetCurrentTexturePalette", 2, get_current_texture_palette),
    ("IDirect3DDevice9::SetScissorRect", 2, set_scissor_rect),
    ("IDirect3DDevice9::GetScissorRect", 2, get_scissor_rect),
    ("IDirect3DDevice9::SetSoftwareVertexProcessing", 2, set_software_vertex_processing),
    ("IDirect3DDevice9::GetSoftwareVertexProcessing", 1, get_software_vertex_processing),
    ("IDirect3DDevice9::SetNPatchMode", 2, set_npatch_mode),
    ("IDirect3DDevice9::GetNPatchMode", 1, NI),
    ("IDirect3DDevice9::DrawPrimitive", 4, draw_primitive),
    ("IDirect3DDevice9::DrawIndexedPrimitive", 7, draw_indexed_primitive),
    ("IDirect3DDevice9::DrawPrimitiveUP", 5, draw_primitive_up),
    ("IDirect3DDevice9::DrawIndexedPrimitiveUP", 9, draw_indexed_primitive_up),
    ("IDirect3DDevice9::ProcessVertices", 7, process_vertices),
    ("IDirect3DDevice9::CreateVertexDeclaration", 3, create_vertex_declaration),
    ("IDirect3DDevice9::SetVertexDeclaration", 2, set_vertex_declaration),
    ("IDirect3DDevice9::GetVertexDeclaration", 2, get_vertex_declaration),
    ("IDirect3DDevice9::SetFVF", 2, set_fvf),
    ("IDirect3DDevice9::GetFVF", 2, get_fvf),
    ("IDirect3DDevice9::CreateVertexShader", 3, create_vertex_shader),
    ("IDirect3DDevice9::SetVertexShader", 2, set_vertex_shader),
    ("IDirect3DDevice9::GetVertexShader", 2, get_vertex_shader),
    ("IDirect3DDevice9::SetVertexShaderConstantF", 4, set_vs_const_f),
    ("IDirect3DDevice9::GetVertexShaderConstantF", 4, get_vs_const_f),
    ("IDirect3DDevice9::SetVertexShaderConstantI", 4, set_vs_const_i),
    ("IDirect3DDevice9::GetVertexShaderConstantI", 4, get_vs_const_i),
    ("IDirect3DDevice9::SetVertexShaderConstantB", 4, set_vs_const_b),
    ("IDirect3DDevice9::GetVertexShaderConstantB", 4, get_vs_const_b),
    ("IDirect3DDevice9::SetStreamSource", 5, set_stream_source),
    ("IDirect3DDevice9::GetStreamSource", 5, get_stream_source),
    ("IDirect3DDevice9::SetStreamSourceFreq", 3, set_stream_source_freq),
    ("IDirect3DDevice9::GetStreamSourceFreq", 3, get_stream_source_freq),
    ("IDirect3DDevice9::SetIndices", 2, set_indices),
    ("IDirect3DDevice9::GetIndices", 2, get_indices),
    ("IDirect3DDevice9::CreatePixelShader", 3, create_pixel_shader),
    ("IDirect3DDevice9::SetPixelShader", 2, set_pixel_shader),
    ("IDirect3DDevice9::GetPixelShader", 2, get_pixel_shader),
    ("IDirect3DDevice9::SetPixelShaderConstantF", 4, set_ps_const_f),
    ("IDirect3DDevice9::GetPixelShaderConstantF", 4, get_ps_const_f),
    ("IDirect3DDevice9::SetPixelShaderConstantI", 4, set_ps_const_i),
    ("IDirect3DDevice9::GetPixelShaderConstantI", 4, get_ps_const_i),
    ("IDirect3DDevice9::SetPixelShaderConstantB", 4, set_ps_const_b),
    ("IDirect3DDevice9::GetPixelShaderConstantB", 4, get_ps_const_b),
    ("IDirect3DDevice9::DrawRectPatch", 4, NI),
    ("IDirect3DDevice9::DrawTriPatch", 4, NI),
    ("IDirect3DDevice9::DeletePatch", 2, NI),
    ("IDirect3DDevice9::CreateQuery", 3, create_query),
];
