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
    // rounding to nearest, all exceptions masked; reserved and infinity bits kept
    assert_eq!(fpu_after_create_device(0x0e7f, 0), 0x007f);
    assert_eq!(fpu_after_create_device(0x0c30, 0), 0x003f);
    assert_eq!(fpu_after_create_device(0x1f72, 0), 0x107f);
    assert_eq!(fpu_after_create_device(0x0c30, D3DCREATE_FPU_PRESERVE), 0x0c30);
}

#[test]
fn back_buffer_format_for_host() {
    use rust_metal::platform::d3d9::host_back_buffer_format as f;
    const R5G6B5: u32 = 23;
    const X8R8G8B8: u32 = 22;
    // fullscreen 16-bit without 16-bit display modes: the 32-bit display's format
    assert_eq!(f(R5G6B5, false, true), X8R8G8B8);
    // windowed, or the adapter has 16-bit modes, or another format: as the game asks
    assert_eq!(f(R5G6B5, true, true), R5G6B5);
    assert_eq!(f(R5G6B5, false, false), R5G6B5);
    assert_eq!(f(X8R8G8B8, false, true), X8R8G8B8);
    assert_eq!(f(25, false, true), 25);
}

#[test]
fn lock_sizes() {
    use rust_metal::platform::d3d9::resources::{lock_bytes, row_bytes};
    const DXT1: u32 = 0x3154_5844;
    const DXT3: u32 = 0x3354_5844;
    assert_eq!(row_bytes(21, 10), Some(40));
    assert_eq!(row_bytes(23, 10), Some(20));
    assert_eq!(row_bytes(DXT1, 64), Some(128));
    assert_eq!(row_bytes(DXT3, 6), Some(32));
    assert_eq!(row_bytes(0x1234, 10), None);
    // 10x3 A8R8G8B8 of a surface with pitch 256: 2 whole rows and 40 bytes
    assert_eq!(lock_bytes(21, 10, 3, 256), Some(2 * 256 + 40));
    // DXT3 64x64, pitch 256 (16 blocks of 16 bytes): 16 block rows
    assert_eq!(lock_bytes(DXT3, 64, 64, 256), Some(16 * 256));
    // a 4x8 part of a DXT1 surface: 2 block rows, the last 8 bytes
    assert_eq!(lock_bytes(DXT1, 4, 8, 128), Some(128 + 8));
    assert_eq!(lock_bytes(21, 10, 0, 256), Some(0));
    assert_eq!(lock_bytes(0x1234, 10, 3, 256), None);
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
