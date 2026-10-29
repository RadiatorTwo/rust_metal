//! Direct3D 9 structures between the game's 32-bit layout and the host's, and the x87 control
//! word after CreateDevice. Needs no game files and no GPU.
#![cfg(windows)]

use rust_metal::platform::d3d9::types::{present_params_from_guest, present_params_to_guest};
use rust_metal::platform::d3d9::{D3DCREATE_FPU_PRESERVE, fpu_after_create_device};
use rust_metal::platform::{alloc, init_memory};

/// What the guest window 0x10002 stands for on the host in this test.
const FAKE_HWND: isize = 0x1234_5678_9abc;

#[test]
fn present_params_layout() {
    let m = init_memory();
    let a = alloc(56);
    let b = alloc(56);
    for (off, v) in [(0, 640), (4, 480), (8, 22), (12, 1), (24, 1), (28, 0x10002), (32, 1), (36, 1), (40, 80), (52, 0x8000_0000)] {
        m.wr32(a + off, v);
    }
    let p = present_params_from_guest(m, a, |g| if g == 0x10002 { FAKE_HWND } else { 0 });
    assert_eq!((p.BackBufferWidth, p.BackBufferHeight), (640, 480));
    assert_eq!(p.BackBufferFormat.0, 22);
    assert_eq!(p.hDeviceWindow.0 as isize, FAKE_HWND);
    assert_eq!(p.Windowed.0, 1);
    assert_eq!(p.AutoDepthStencilFormat.0, 80);
    assert_eq!(p.PresentationInterval, 0x8000_0000);
    present_params_to_guest(m, b, &p, |h| if h == FAKE_HWND { 0x10002 } else { 0 });
    assert_eq!(m.slice(a, 56).to_vec(), m.slice(b, 56).to_vec());
}

#[test]
fn fpu_after_create() {
    assert_eq!(fpu_after_create_device(0x027f, 0), 0x007f);
    assert_eq!(fpu_after_create_device(0x027f, D3DCREATE_FPU_PRESERVE), 0x027f);
    assert_eq!(fpu_after_create_device(0x037f, 0x40), 0x007f);
}

#[test]
fn caps_same_size() {
    // D3DCAPS9 has no pointers: 304 bytes on 32 and 64 bits, copied as it is
    assert_eq!(std::mem::size_of::<windows::Win32::Graphics::Direct3D9::D3DCAPS9>(), 304);
}

#[test]
fn vtable_sizes() {
    use rust_metal::platform::d3d9::{IDIRECT3D9, device};
    assert_eq!(IDIRECT3D9.len(), 17);
    assert_eq!(device::METHODS.len(), 119);
    assert_eq!(device::METHODS[17].0, "IDirect3DDevice9::Present");
    assert_eq!(device::METHODS[43].0, "IDirect3DDevice9::Clear");
    assert_eq!(device::METHODS[118].0, "IDirect3DDevice9::CreateQuery");
}
