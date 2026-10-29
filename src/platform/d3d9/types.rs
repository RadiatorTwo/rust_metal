//! Direct3D 9 structures that differ between the game's 32-bit layout and the host's: the ones
//! with a window handle (`D3DPRESENT_PARAMETERS`, `D3DDEVICE_CREATION_PARAMETERS`). The rest
//! (D3DCAPS9, D3DDISPLAYMODE, D3DVIEWPORT9, matrices, ...) hold only 32-bit values and are
//! copied as they are; `D3DADAPTER_IDENTIFIER9` is the host's without its 4 bytes of padding
//! at the end (32-bit d3d9types.h packs to 4: 1100 bytes, the host's 1104).

use crate::rt::Memory;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct3D9::*;
use windows::core::BOOL;

/// The game's `D3DPRESENT_PARAMETERS`: 14 dwords, hDeviceWindow (a guest window) at 28.
pub const PRESENT_PARAMS_SIZE: u32 = 56;

/// The game's `D3DPRESENT_PARAMETERS` at `a`, its window mapped by `hwnd`.
pub fn present_params_from_guest(m: &Memory, a: u32, hwnd: impl Fn(u32) -> isize) -> D3DPRESENT_PARAMETERS {
    let d = |k: u32| m.rd32(a + 4 * k);
    D3DPRESENT_PARAMETERS {
        BackBufferWidth: d(0),
        BackBufferHeight: d(1),
        BackBufferFormat: D3DFORMAT(d(2)),
        BackBufferCount: d(3),
        MultiSampleType: D3DMULTISAMPLE_TYPE(d(4) as i32),
        MultiSampleQuality: d(5),
        SwapEffect: D3DSWAPEFFECT(d(6) as i32),
        hDeviceWindow: HWND(hwnd(d(7)) as *mut _),
        Windowed: BOOL(d(8) as i32),
        EnableAutoDepthStencil: BOOL(d(9) as i32),
        AutoDepthStencilFormat: D3DFORMAT(d(10)),
        Flags: d(11),
        FullScreen_RefreshRateInHz: d(12),
        PresentationInterval: d(13),
    }
}

/// Write `p` back to the game's `D3DPRESENT_PARAMETERS` at `a` (D3D9 fills in what the game
/// left 0: back buffer size, format and count), its window mapped by `guest_hwnd`.
pub fn present_params_to_guest(m: &Memory, a: u32, p: &D3DPRESENT_PARAMETERS, guest_hwnd: impl Fn(isize) -> u32) {
    let v = [
        p.BackBufferWidth,
        p.BackBufferHeight,
        p.BackBufferFormat.0,
        p.BackBufferCount,
        p.MultiSampleType.0 as u32,
        p.MultiSampleQuality,
        p.SwapEffect.0 as u32,
        guest_hwnd(p.hDeviceWindow.0 as isize),
        p.Windowed.0 as u32,
        p.EnableAutoDepthStencil.0 as u32,
        p.AutoDepthStencilFormat.0,
        p.Flags,
        p.FullScreen_RefreshRateInHz,
        p.PresentationInterval,
    ];
    for (k, x) in v.into_iter().enumerate() {
        m.wr32(a + 4 * k as u32, x);
    }
}

/// Bytes of the game's `D3DADAPTER_IDENTIFIER9`.
pub const ADAPTER_IDENTIFIER_SIZE: usize = 1100;

/// A plain structure's bytes (no pointers in it).
pub fn bytes_of<T: Copy>(v: &T) -> &[u8] {
    unsafe { std::slice::from_raw_parts(v as *const T as *const u8, std::mem::size_of::<T>()) }
}

/// A plain structure from guest bytes at `a` (no pointers in it).
pub fn read_guest<T: Copy + Default>(m: &Memory, a: u32) -> T {
    let mut v = T::default();
    let n = std::mem::size_of::<T>();
    unsafe { std::slice::from_raw_parts_mut(&mut v as *mut T as *mut u8, n) }.copy_from_slice(m.slice(a, n));
    v
}

/// Write a plain structure's first `n` bytes to the guest at `a`.
pub fn write_guest<T: Copy>(m: &Memory, a: u32, v: &T, n: usize) {
    m.slice(a, n).copy_from_slice(&bytes_of(v)[..n]);
}
