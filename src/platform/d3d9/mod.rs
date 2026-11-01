//! d3d9.dll, passed through to the system's. The game loads it with LoadLibraryA (`d3d9.dll`,
//! else `d3d9d.dll`) and asks for `Direct3DCreate9` (d3d_create, 0x469530: the first thing the
//! game does after the C runtime, its CPU detection and CoInitialize, before its window exists).
//!
//! Every interface the game holds is a guest COM object (`com::wrap`) whose vtable is ours, in
//! the SDK's method order; each method finds the host interface behind `this` and calls the
//! real one. Structures without pointers are passed in place (guest memory is host memory at
//! `mem().base`); those with a window handle are converted (`types`). Methods we do not
//! implement return 0 (D3D_OK) and are logged at their first call by name.
//!
//! What the game asks for (adapters, modes, caps, the device's mode and windowed/fullscreen)
//! goes through unchanged. CreateDevice adds D3DCREATE_MULTITHREADED (the game's threads call
//! in without a turn held) and sets the calling thread's x87 control word as real D3D9 does
//! on 32-bit Windows (`fpu_after_create_device`).

pub mod device;
pub mod ex;
pub mod queue;
pub mod resources;
pub mod state;
pub mod shot;
pub mod types;

use super::{Import, arg, com, log, mem, method_not_implemented as NI};
use crate::rt::Cpu;
use std::ffi::c_void;
use types::*;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct3D9::*;
use windows::core::{GUID, IUnknown_Vtbl, Interface};

/// The CPU's clock rate the game measured (cpu_mhz, 0x563350).
const CPU_MHZ: u32 = 0x76c474;

pub const D3DCREATE_FPU_PRESERVE: u32 = 0x2;
pub const D3DCREATE_MULTITHREADED: u32 = 0x4;

/// The x87 control word after a successful CreateDevice with `flags`: without
/// D3DCREATE_FPU_PRESERVE real D3D9 (and Wine) sets single precision, rounding to nearest and
/// all exceptions masked on the calling thread and leaves it so; the game's arithmetic after
/// that is 24-bit.
pub fn fpu_after_create_device(cw: u16, flags: u32) -> u16 {
    if flags & D3DCREATE_FPU_PRESERVE != 0 { cw } else { (cw & !0x0f3f) | 0x003f }
}

/// A host pointer to guest memory at `a` (null for 0), for structures with the same layout.
/// The structure's pages are marked as written (`rt::MAP_OFF`): Direct3D may fill it.
pub(crate) fn gp<T>(a: u32) -> *mut T {
    if a == 0 {
        return std::ptr::null_mut();
    }
    mem().mark(a, std::mem::size_of::<T>());
    unsafe { mem().base.add(a as usize) as *mut T }
}

/// A host pointer to guest memory at `a` for reading (null for 0).
pub(crate) fn cp<T>(a: u32) -> *const T {
    gp::<T>(a)
}

/// The vtable of a host interface.
pub(crate) unsafe fn vtbl<V>(p: *mut c_void) -> &'static V {
    unsafe { &**(p as *const *const V) }
}

/// An HRESULT as eax.
pub(crate) fn hr(h: windows::core::HRESULT) -> u32 {
    h.0 as u32
}

pub(crate) const E_NOINTERFACE: u32 = 0x8000_4002;

/// QueryInterface for an interface we wrap: IUnknown or `own` gives the same object, anything
/// else E_NOINTERFACE (logged).
pub(crate) fn query_interface(c: &mut Cpu, own: GUID, name: &str) -> u32 {
    let (this, riid, ppv) = (arg(c, 0), arg(c, 1), arg(c, 2));
    let g: GUID = read_guest(mem(), riid);
    if g == own || g == windows::core::IUnknown::IID {
        add_ref(c);
        mem().wr32(ppv, this);
        return 0;
    }
    log!("warn: {name}::QueryInterface({g:?}): not wrapped (E_NOINTERFACE)");
    if ppv != 0 {
        mem().wr32(ppv, 0);
    }
    E_NOINTERFACE
}

/// IUnknown::AddRef of any wrapped interface: the host's count.
pub(crate) fn add_ref(c: &mut Cpu) -> u32 {
    let p = com::host(arg(c, 0));
    unsafe { (vtbl::<IUnknown_Vtbl>(p).AddRef)(p) }
}

/// IUnknown::Release of any wrapped interface: the host's count; at 0 the guest object goes.
pub(crate) fn release(c: &mut Cpu) -> u32 {
    let this = arg(c, 0);
    let p = com::host(this);
    let n = unsafe { (vtbl::<IUnknown_Vtbl>(p).Release)(p) };
    com::release(this, n);
    n
}

/// Direct3DCreate9(SDK version): the system's IDirect3D9 (the game asks for 31, D3D9b).
fn direct3d_create9(c: &mut Cpu) -> u32 {
    let sdk = arg(c, 0);
    log!("Direct3DCreate9({sdk}) (the game measured {} MHz)", mem().rd32(CPU_MHZ));
    if let Some(d) = ex::create(sdk) {
        return com::wrap(IDIRECT3D9, d);
    }
    match unsafe { Direct3DCreate9(sdk) } {
        Some(d) => com::wrap(IDIRECT3D9, d.into_raw()),
        None => {
            log!("warn: Direct3DCreate9({sdk}) failed");
            0
        }
    }
}

fn d3d(c: &Cpu) -> (*mut c_void, &'static IDirect3D9_Vtbl) {
    let p = com::host(arg(c, 0));
    (p, unsafe { vtbl(p) })
}

fn qi(c: &mut Cpu) -> u32 {
    query_interface(c, IDirect3D9::IID, "IDirect3D9")
}

fn get_adapter_count(c: &mut Cpu) -> u32 {
    let (p, v) = d3d(c);
    unsafe { (v.GetAdapterCount)(p) }
}

fn get_adapter_identifier(c: &mut Cpu) -> u32 {
    let (p, v) = d3d(c);
    let mut id = D3DADAPTER_IDENTIFIER9::default();
    let r = unsafe { (v.GetAdapterIdentifier)(p, arg(c, 1), arg(c, 2), &mut id) };
    if r.is_ok() {
        write_guest(mem(), arg(c, 3), &id, ADAPTER_IDENTIFIER_SIZE);
        let desc: String = id.Description.iter().take_while(|&&b| b != 0).map(|&b| b as u8 as char).collect();
        log!("d3d9: adapter {}: {desc} (vendor {:#06x} device {:#06x})", arg(c, 1), id.VendorId, id.DeviceId);
    }
    hr(r)
}

// ---- 16-bit display modes ----
//
// The game wants every resolution in two colour depths: game_start's mode list (0x469760,
// 0x469e20) takes the adapter's modes of X1R5G5B5, A1R5G5B5, R5G6B5, X8R8G8B8 and A8R8G8B8,
// and 0x46afd0 drops a resolution that has not exactly two of them (Windows XP drivers had
// R5G6B5 and X8R8G8B8). Windows 8 and later have no 16-bit display modes: no resolution
// would be left ("Nicht unterstützte Grafikkarte."). When the adapter has no R5G6B5 modes,
// they are offered as its X8R8G8B8 ones, and R5G6B5 as the display format (in checks and
// fullscreen devices) means X8R8G8B8, which Windows emulates 16-bit with anyway.

const R5G6B5: u32 = 23;
const X8R8G8B8: u32 = 22;

/// The adapter has no R5G6B5 display modes (Windows 8 and later): they are emulated. Asked
/// once per adapter.
fn no_16bit_modes(p: *mut c_void, v: &IDirect3D9_Vtbl, adapter: u32) -> bool {
    static KNOWN: std::sync::Mutex<Vec<(u32, bool)>> = std::sync::Mutex::new(Vec::new());
    if let Some(&(_, n)) = super::lock(&KNOWN).iter().find(|e| e.0 == adapter) {
        return n;
    }
    let n = unsafe { (v.GetAdapterModeCount)(p, adapter, D3DFORMAT(R5G6B5)) == 0 };
    super::lock(&KNOWN).push((adapter, n));
    n
}

/// The device's adapter has no 16-bit display modes (for Reset).
static DEVICE_NO_16BIT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The back buffer format for the host: fullscreen, the back buffer has the display's format,
/// and an emulated 16-bit display (no R5G6B5 modes) is X8R8G8B8.
pub fn host_back_buffer_format(format: u32, windowed: bool, no_16bit_modes: bool) -> u32 {
    if format == R5G6B5 && !windowed && no_16bit_modes { X8R8G8B8 } else { format }
}

/// CreateDevice or Reset (`what`, `call`) with the game's present parameters at `pp`: the
/// back buffer format as the host's (`host_back_buffer_format`), the results written back
/// with the game's own format. The host's device is always windowed: what the game asks for
/// as fullscreen is a back buffer of that size shown in our window, which is a window or
/// borderless fullscreen (Alt+Enter), never exclusive fullscreen (the display mode stays the
/// desktop's); the game sees its own fullscreen parameters back. Fullscreen, input positions
/// are in back buffer pixels; back in a window, in the window's.
pub(crate) fn with_present_params(pp: u32, no_16bit: bool, what: &str, call: impl FnOnce(&mut D3DPRESENT_PARAMETERS) -> windows::core::HRESULT) -> windows::core::HRESULT {
    let m = mem();
    let mut params = present_params_from_guest(m, pp, super::window::host_hwnd);
    log!("d3d9: {what}: {}", describe(&params));
    let game_format = params.BackBufferFormat.0;
    let (game_windowed, game_refresh) = (params.Windowed, params.FullScreen_RefreshRateInHz);
    let host_format = host_back_buffer_format(game_format, game_windowed.0 != 0, no_16bit);
    params.BackBufferFormat = D3DFORMAT(host_format);
    params.Windowed = windows::core::BOOL(1);
    params.FullScreen_RefreshRateInHz = 0;
    let r = call(&mut params);
    if params.BackBufferFormat.0 == host_format {
        params.BackBufferFormat = D3DFORMAT(game_format);
    }
    (params.Windowed, params.FullScreen_RefreshRateInHz) = (game_windowed, game_refresh);
    present_params_to_guest(m, pp, &params, super::window::guest_hwnd);
    if r.is_err() {
        log!("warn: {what} failed: {:#x}", r.0);
        return r;
    }
    BACK_BUFFER.store((params.BackBufferWidth as u64) << 32 | params.BackBufferHeight as u64, std::sync::atomic::Ordering::Relaxed);
    if game_windowed.0 == 0 {
        super::window::set_game_size(params.BackBufferWidth, params.BackBufferHeight);
    } else {
        super::window::windowed_game_size();
    }
    r
}

/// The device's back buffer size (width << 32 | height), for Present.
static BACK_BUFFER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Where Present puts the back buffer in the window `hwnd`: its picture rectangle
/// (`window::picture`, the aspect ratio kept); the bars around it are painted black. None
/// when the picture fills the client area or there is none.
pub(crate) fn letterbox(hwnd: HWND) -> Option<windows::Win32::Foundation::RECT> {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::Graphics::Gdi::{BLACK_BRUSH, FillRect, GetDC, GetStockObject, HBRUSH, ReleaseDC};
    let mut client = windows_sys::Win32::Foundation::RECT { left: 0, top: 0, right: 0, bottom: 0 };
    if hwnd.is_invalid() || unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd.0, &mut client) } == 0 {
        return None;
    }
    let (cw, ch) = (client.right.max(0), client.bottom.max(0));
    let v = BACK_BUFFER.load(std::sync::atomic::Ordering::Relaxed);
    let back = ((v >> 32) as u32, v as u32);
    if cw == 0 || ch == 0 || back.0 == 0 || back.1 == 0 {
        return None;
    }
    let [x, y, w, h] = super::window::picture((cw as u32, ch as u32), back);
    if (w, h) == (cw, ch) {
        return None;
    }
    // the bars: left and right, or above and below
    let bars = if x > 0 {
        [RECT { left: 0, top: 0, right: x, bottom: ch }, RECT { left: x + w, top: 0, right: cw, bottom: ch }]
    } else {
        [RECT { left: 0, top: 0, right: cw, bottom: y }, RECT { left: 0, top: y + h, right: cw, bottom: ch }]
    };
    unsafe {
        let dc = GetDC(Some(hwnd));
        if !dc.is_invalid() {
            for r in &bars {
                FillRect(dc, r, HBRUSH(GetStockObject(BLACK_BRUSH).0));
            }
            ReleaseDC(Some(hwnd), dc);
        }
    }
    Some(RECT { left: x, top: y, right: x + w, bottom: y + h })
}

/// A display format the game asks about, as the host's.
fn display_format(p: *mut c_void, v: &IDirect3D9_Vtbl, adapter: u32, f: u32) -> D3DFORMAT {
    D3DFORMAT(if f == R5G6B5 && no_16bit_modes(p, v, adapter) { X8R8G8B8 } else { f })
}

fn get_adapter_mode_count(c: &mut Cpu) -> u32 {
    let (p, v) = d3d(c);
    let adapter = arg(c, 1);
    unsafe { (v.GetAdapterModeCount)(p, adapter, display_format(p, v, adapter, arg(c, 2))) }
}

fn enum_adapter_modes(c: &mut Cpu) -> u32 {
    let (p, v) = d3d(c);
    let (adapter, f, out) = (arg(c, 1), arg(c, 2), arg(c, 4));
    let r = unsafe { (v.EnumAdapterModes)(p, adapter, display_format(p, v, adapter, f), arg(c, 3), gp(out)) };
    if r.is_ok() {
        // D3DDISPLAYMODE: width, height, refresh rate, format
        mem().wr32(out + 12, f);
    }
    hr(r)
}

fn get_adapter_display_mode(c: &mut Cpu) -> u32 {
    let (p, v) = d3d(c);
    hr(unsafe { (v.GetAdapterDisplayMode)(p, arg(c, 1), gp(arg(c, 2))) })
}

fn check_device_type(c: &mut Cpu) -> u32 {
    let (p, v) = d3d(c);
    let a = |k| arg(c, k);
    let display = display_format(p, v, a(1), a(3));
    // fullscreen, the back buffer has the display's format
    let back = if a(5) == 0 && a(4) == a(3) { display } else { D3DFORMAT(a(4)) };
    hr(unsafe { (v.CheckDeviceType)(p, a(1), D3DDEVTYPE(a(2) as i32), display, back, windows::core::BOOL(a(5) as i32)) })
}

fn check_device_format(c: &mut Cpu) -> u32 {
    let (p, v) = d3d(c);
    let a = |k| arg(c, k);
    hr(unsafe {
        (v.CheckDeviceFormat)(p, a(1), D3DDEVTYPE(a(2) as i32), display_format(p, v, a(1), a(3)), a(4), D3DRESOURCETYPE(a(5) as i32), D3DFORMAT(a(6)))
    })
}

fn check_device_multi_sample_type(c: &mut Cpu) -> u32 {
    let (p, v) = d3d(c);
    let a = |k| arg(c, k);
    hr(unsafe {
        (v.CheckDeviceMultiSampleType)(
            p,
            a(1),
            D3DDEVTYPE(a(2) as i32),
            D3DFORMAT(a(3)),
            windows::core::BOOL(a(4) as i32),
            D3DMULTISAMPLE_TYPE(a(5) as i32),
            gp(a(6)),
        )
    })
}

fn check_depth_stencil_match(c: &mut Cpu) -> u32 {
    let (p, v) = d3d(c);
    let a = |k| arg(c, k);
    hr(unsafe { (v.CheckDepthStencilMatch)(p, a(1), D3DDEVTYPE(a(2) as i32), display_format(p, v, a(1), a(3)), D3DFORMAT(a(4)), D3DFORMAT(a(5))) })
}

fn check_device_format_conversion(c: &mut Cpu) -> u32 {
    let (p, v) = d3d(c);
    let a = |k| arg(c, k);
    hr(unsafe { (v.CheckDeviceFormatConversion)(p, a(1), D3DDEVTYPE(a(2) as i32), D3DFORMAT(a(3)), D3DFORMAT(a(4))) })
}

fn get_device_caps(c: &mut Cpu) -> u32 {
    let (p, v) = d3d(c);
    hr(unsafe { (v.GetDeviceCaps)(p, arg(c, 1), D3DDEVTYPE(arg(c, 2) as i32), gp(arg(c, 3))) })
}

/// The host window for a guest one (the game's main window is the winit window).
pub(crate) fn host_hwnd(guest: u32) -> HWND {
    HWND(super::window::host_hwnd(guest) as *mut _)
}

/// CreateDevice(adapter, type, focus window, behaviour flags, present parameters, out device).
fn create_device(c: &mut Cpu) -> u32 {
    let (p, v) = d3d(c);
    let (adapter, ty, focus, flags, pp, out) = (arg(c, 1), arg(c, 2), arg(c, 3), arg(c, 4), arg(c, 5), arg(c, 6));
    let m = mem();
    let no_16bit = no_16bit_modes(p, v, adapter);
    let what = format!("CreateDevice(adapter {adapter}, type {ty}, focus {focus:#x}, flags {flags:#x})");
    let mut dev: *mut c_void = std::ptr::null_mut();
    let r = with_present_params(pp, no_16bit, &what, |params| unsafe {
        let (ty, focus, flags) = (D3DDEVTYPE(ty as i32), host_hwnd(focus), flags | D3DCREATE_MULTITHREADED);
        if !ex::on() {
            return (v.CreateDevice)(p, adapter, ty, focus, flags, params, &mut dev);
        }
        let asked = ex::host_params(params);
        let r = (vtbl::<IDirect3D9Ex_Vtbl>(p).CreateDeviceEx)(p, adapter, ty, focus, flags, params, std::ptr::null_mut(), &mut dev);
        ex::game_params(params, &asked);
        if r.is_err() {
            return r;
        }
        let t = ex::make_target(dev, params);
        if t.is_err() {
            (vtbl::<IUnknown_Vtbl>(dev).Release)(dev);
            dev = std::ptr::null_mut();
        }
        t
    });
    if r.is_err() {
        m.wr32(out, 0);
        return hr(r);
    }
    device::GAME_FLAGS.store(flags, std::sync::atomic::Ordering::Relaxed);
    DEVICE_NO_16BIT.store(no_16bit, std::sync::atomic::Ordering::Relaxed);
    c.fcw = fpu_after_create_device(c.fcw, flags);
    DEVICE.store(dev as usize, std::sync::atomic::Ordering::Relaxed);
    m.wr32(out, com::wrap(device::METHODS, dev));
    device::invalidate_states();
    device::device_created();
    queue::start();
    0
}

/// The host device the game created last (null before CreateDevice).
static DEVICE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// The host IDirect3DDevice9 (null before the game created it), for drawing of our own.
pub fn device() -> *mut c_void {
    DEVICE.load(std::sync::atomic::Ordering::Relaxed) as *mut c_void
}

/// The device's Release returned 0: `device()` is null again.
pub(crate) fn device_released(host: *mut c_void) {
    let _ = DEVICE.compare_exchange(host as usize, 0, std::sync::atomic::Ordering::Relaxed, std::sync::atomic::Ordering::Relaxed);
}

/// Whether the device's adapter has its 16-bit display modes emulated.
pub(crate) fn device_no_16bit_modes() -> bool {
    DEVICE_NO_16BIT.load(std::sync::atomic::Ordering::Relaxed)
}

/// Present parameters for the log.
pub(crate) fn describe(p: &D3DPRESENT_PARAMETERS) -> String {
    format!(
        "{}x{} format {} x{} multisample {}/{} swap effect {} {} hwnd {:#x} depth {} format {} flags {:#x} {} Hz interval {:#x}",
        p.BackBufferWidth,
        p.BackBufferHeight,
        p.BackBufferFormat.0,
        p.BackBufferCount,
        p.MultiSampleType.0,
        p.MultiSampleQuality,
        p.SwapEffect.0,
        if p.Windowed.0 != 0 { "windowed" } else { "fullscreen" },
        p.hDeviceWindow.0 as isize,
        p.EnableAutoDepthStencil.0,
        p.AutoDepthStencilFormat.0,
        p.Flags,
        p.FullScreen_RefreshRateInHz,
        p.PresentationInterval
    )
}

/// IDirect3D9, in the SDK's order (17 methods).
pub static IDIRECT3D9: &[Import] = &[
    ("IDirect3D9::QueryInterface", 3, qi),
    ("IDirect3D9::AddRef", 1, add_ref),
    ("IDirect3D9::Release", 1, release),
    ("IDirect3D9::RegisterSoftwareDevice", 2, NI),
    ("IDirect3D9::GetAdapterCount", 1, get_adapter_count),
    ("IDirect3D9::GetAdapterIdentifier", 4, get_adapter_identifier),
    ("IDirect3D9::GetAdapterModeCount", 3, get_adapter_mode_count),
    ("IDirect3D9::EnumAdapterModes", 5, enum_adapter_modes),
    ("IDirect3D9::GetAdapterDisplayMode", 3, get_adapter_display_mode),
    ("IDirect3D9::CheckDeviceType", 6, check_device_type),
    ("IDirect3D9::CheckDeviceFormat", 7, check_device_format),
    ("IDirect3D9::CheckDeviceMultiSampleType", 7, check_device_multi_sample_type),
    ("IDirect3D9::CheckDepthStencilMatch", 6, check_depth_stencil_match),
    ("IDirect3D9::CheckDeviceFormatConversion", 5, check_device_format_conversion),
    ("IDirect3D9::GetDeviceCaps", 4, get_device_caps),
    ("IDirect3D9::GetAdapterMonitor", 2, NI),
    ("IDirect3D9::CreateDevice", 7, create_device),
];

pub const IMPORTS: &[Import] = &[("Direct3DCreate9", 1, direct3d_create9)];
