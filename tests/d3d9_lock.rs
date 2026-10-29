//! Direct3D 9 locks between guest buffers and host memory, with a fake host resource (plain
//! host memory). Needs no game files and no GPU.
#![cfg(windows)]

use rust_metal::platform::d3d9::resources::{
    D3DLOCK_DISCARD, D3DLOCK_NOOVERWRITE, D3DLOCK_READONLY, begin_lock, end_lock, forget, lock_copy_in, primitive_vertices, rect_offset, vertex_elements,
};
use rust_metal::platform::{alloc, init_memory};

#[test]
fn copy_in_flags() {
    assert!(lock_copy_in(0));
    assert!(lock_copy_in(D3DLOCK_READONLY));
    assert!(!lock_copy_in(D3DLOCK_DISCARD));
    assert!(lock_copy_in(D3DLOCK_NOOVERWRITE));
}

/// CreateVertexDeclaration's array: elements up to and with the end marker (stream 0xff).
#[test]
fn vertex_elements_end() {
    let m = init_memory();
    let a = alloc(40);
    // position (float3) at 0, normal at 12, texcoord at 24; then D3DDECL_END
    for (k, (off, ty, usage)) in [(0u16, 2u8, 0u8), (12, 2, 3), (24, 1, 5)].into_iter().enumerate() {
        let e = a + 8 * k as u32;
        m.wr16(e, 0);
        m.wr16(e + 2, off);
        m.slice(e + 4, 4).copy_from_slice(&[ty, 0, usage, 0]);
    }
    m.wr16(a + 24, 0xff);
    m.wr16(a + 26, 0);
    m.slice(a + 28, 4).copy_from_slice(&[17, 0, 0, 0]);
    // garbage after the end is not read
    m.slice(a + 32, 8).fill(0x77);
    let v = vertex_elements(a);
    assert_eq!(v.len(), 4);
    assert_eq!((v[1].Offset, v[1].Type, v[1].Usage), (12, 2, 3));
    assert_eq!((v[3].Stream, v[3].Type), (0xff, 17));
}

/// Where a rect lock starts in a level's guest buffer of `pitch` bytes per row.
#[test]
fn rect_offsets() {
    // A8R8G8B8: 2 rows and 3 pixels in
    assert_eq!(rect_offset(21, 3, 2, 256), Some(2 * 256 + 12));
    // DXT1: the row of blocks at y 4, the block at x 8 (8 bytes a block)
    assert_eq!(rect_offset(0x3154_5844, 8, 4, 64), Some(64 + 16));
    // DXT5: 16 bytes a block
    assert_eq!(rect_offset(0x3554_5844, 4, 8, 128), Some(2 * 128 + 16));
    assert_eq!(rect_offset(0x1234_5678, 0, 0, 64), None);
}

/// Vertices a draw of `count` primitives reads (for *UP draws).
#[test]
fn up_vertex_counts() {
    assert_eq!(primitive_vertices(1, 5), 5); // points
    assert_eq!(primitive_vertices(2, 5), 10); // lines
    assert_eq!(primitive_vertices(3, 5), 6); // line strip
    assert_eq!(primitive_vertices(4, 5), 15); // triangles
    assert_eq!(primitive_vertices(5, 5), 7); // strip
    assert_eq!(primitive_vertices(6, 5), 7); // fan
    assert_eq!(primitive_vertices(5, 0), 0);
}

/// Two nested locks of one buffer: each Unlock copies its own range back.
#[test]
fn nested_locks() {
    let m = init_memory();
    let mut host: Vec<u8> = (0..32).collect();
    let base = host.as_mut_ptr();
    let shadow = alloc(32);
    let this = alloc(8); // stands for the guest object
    begin_lock(this, 0, shadow, base as *mut _, 16, 0);
    begin_lock(this, 0, shadow + 16, unsafe { base.add(16) } as *mut _, 16, 0);
    // copied in
    assert_eq!(m.slice(shadow, 32).to_vec(), (0..32).collect::<Vec<u8>>());
    m.slice(shadow, 32).fill(0xaa);
    // the inner lock first: only its range
    assert!(end_lock(this, 0));
    assert_eq!(&host[..16], &(0..16).collect::<Vec<u8>>()[..]);
    assert_eq!(&host[16..], &[0xaa; 16]);
    // then the outer one
    assert!(end_lock(this, 0));
    assert_eq!(&host[..], &[0xaa; 32]);
    assert!(!end_lock(this, 0));
}

/// A read-only lock does not write back; a discarding one does not copy in.
#[test]
fn readonly_and_discard() {
    let m = init_memory();
    let mut host = vec![7u8; 8];
    let g = alloc(8);
    let this = alloc(8);
    m.slice(g, 8).fill(0);
    begin_lock(this, 1, g, host.as_mut_ptr() as *mut _, 8, D3DLOCK_DISCARD);
    assert_eq!(m.slice(g, 8).to_vec(), vec![0; 8]);
    m.slice(g, 8).fill(3);
    end_lock(this, 1);
    assert_eq!(host, vec![3; 8]);
    begin_lock(this, 1, g, host.as_mut_ptr() as *mut _, 8, D3DLOCK_READONLY);
    m.slice(g, 8).fill(9);
    end_lock(this, 1);
    assert_eq!(host, vec![3; 8]);
}

/// A nested lock (without DISCARD) of a range the game already wrote through the outer lock:
/// the outer write survives, as in D3D9, where both pointers are the same memory.
#[test]
fn nested_lock_keeps_outer_writes() {
    let m = init_memory();
    let mut host = vec![1u8; 16];
    let base = host.as_mut_ptr();
    let shadow = alloc(16);
    let this = alloc(8);
    begin_lock(this, 0, shadow, base as *mut _, 16, 0);
    m.slice(shadow + 4, 4).fill(0x55);
    begin_lock(this, 0, shadow + 4, unsafe { base.add(4) } as *mut _, 8, 0);
    assert_eq!(m.slice(shadow + 4, 4).to_vec(), vec![0x55; 4]);
    end_lock(this, 0);
    end_lock(this, 0);
    assert_eq!(&host[..4], &[1; 4]);
    assert_eq!(&host[4..8], &[0x55; 4]);
    assert_eq!(&host[8..], &[1; 8]);
}

// ---- IDirect3DVertexBuffer9::Lock through its guest method, on a fake host buffer ----

use rust_metal::platform::com;
use rust_metal::platform::d3d9::resources::VERTEX_BUFFER;
use rust_metal::rt::{Cpu, ESP};
use std::ffi::c_void;

/// A host vertex buffer of 64 bytes: its vtable and memory.
#[repr(C)]
struct Fake {
    vtbl: *const [usize; 14],
    bytes: [u8; 64],
}

unsafe extern "system" fn fake_lock(this: *mut c_void, offset: u32, _size: u32, out: *mut *mut c_void, _flags: u32) -> i32 {
    unsafe { *out = (*(this as *mut Fake)).bytes.as_mut_ptr().add(offset as usize) as *mut c_void };
    0
}

unsafe extern "system" fn fake_unlock(_: *mut c_void) -> i32 {
    0
}

/// D3DVERTEXBUFFER_DESC: Size at 16.
unsafe extern "system" fn fake_get_desc(_: *mut c_void, desc: *mut u32) -> i32 {
    unsafe { *desc.add(4) = 64 };
    0
}

/// Call a guest method with arguments on a stack in guest memory. (Objects released with
/// `com::release` directly need `forget`, as the Release method does: their address comes back.)
fn call(f: usize, args: &[u32]) -> u32 {
    let m = init_memory();
    let stack = alloc(256);
    let mut c = Cpu::new(m);
    c.r[ESP] = stack + 64;
    for (k, &a) in args.iter().enumerate() {
        m.wr32(stack + 68 + 4 * k as u32, a);
    }
    (VERTEX_BUFFER[f].2)(&mut c)
}

#[test]
fn buffer_lock_offsets() {
    let m = init_memory();
    let mut vt = [0usize; 14];
    vt[11] = fake_lock as *const () as usize;
    vt[12] = fake_unlock as *const () as usize;
    vt[13] = fake_get_desc as *const () as usize;
    let mut fake = Box::new(Fake { vtbl: &vt, bytes: [0; 64] });
    for (k, b) in fake.bytes.iter_mut().enumerate() {
        *b = k as u8;
    }
    let host = &mut *fake as *mut Fake as *mut c_void;
    let vb = com::wrap(VERTEX_BUFFER, host);
    let out = alloc(8);
    // Lock(16, 8): the guest pointer is 16 into a guest copy of the whole buffer
    assert_eq!(call(11, &[vb, 16, 8, out, 0]), 0);
    let p16 = m.rd32(out);
    assert_eq!(m.rd8(p16), 16);
    // Lock(0, 0) nested: the whole buffer, at the start of the same guest copy
    assert_eq!(call(11, &[vb, 0, 0, out + 4, 0]), 0);
    let p0 = m.rd32(out + 4);
    assert_eq!(p0 + 16, p16);
    m.slice(p0, 64).fill(0xee);
    assert_eq!(call(12, &[vb]), 0);
    assert_eq!(fake.bytes, [0xee; 64]);
    // past the end: D3DERR_INVALIDCALL
    assert_eq!(call(11, &[vb, 60, 8, out, 0]), 0x8876_086c);
    assert_eq!(call(12, &[vb]), 0);
    forget(vb);
    com::release(vb, 0);
}

/// A buffer's guest copy is what the host holds between locks: a later lock (NOOVERWRITE or
/// none) does not read the host's (write-combined) memory again.
#[test]
fn buffer_shadow_kept() {
    let m = init_memory();
    let mut vt = [0usize; 14];
    vt[11] = fake_lock as *const () as usize;
    vt[12] = fake_unlock as *const () as usize;
    vt[13] = fake_get_desc as *const () as usize;
    let mut fake = Box::new(Fake { vtbl: &vt, bytes: [1; 64] });
    let host = &mut *fake as *mut Fake as *mut c_void;
    let vb = com::wrap(VERTEX_BUFFER, host);
    let out = alloc(4);
    // the first lock reads the host's bytes
    assert_eq!(call(11, &[vb, 0, 32, out, 0]), 0);
    let p = m.rd32(out);
    assert_eq!(m.slice(p, 32).to_vec(), vec![1; 32]);
    m.slice(p, 32).fill(5);
    assert_eq!(call(12, &[vb]), 0);
    assert_eq!(&fake.bytes[..32], &[5; 32]);
    // what a GPU-side buffer reads back is not what was written (write-combined, undefined)
    fake.bytes.fill(0x99);
    assert_eq!(call(11, &[vb, 0, 0, out, D3DLOCK_NOOVERWRITE]), 0);
    let p = m.rd32(out);
    // the first half is the guest's, the second half was never locked: read now
    assert_eq!(m.slice(p, 32).to_vec(), vec![5; 32]);
    assert_eq!(m.slice(p + 32, 32).to_vec(), vec![0x99; 32]);
    assert_eq!(call(12, &[vb]), 0);
    forget(vb);
    com::release(vb, 0);
}

// ---- IDirect3DTexture9::LockRect: one guest buffer per level ----

use rust_metal::platform::d3d9::resources::TEXTURE;
use windows::Win32::Graphics::Direct3D9::D3DLOCKED_RECT;

/// A host A8R8G8B8 texture of one 16x8 level, pitch 64: its vtable and memory.
#[repr(C)]
struct FakeTex {
    vtbl: *const [usize; 22],
    bytes: [u8; 512],
}

/// D3DSURFACE_DESC: format at 0, width at 24, height at 28.
unsafe extern "system" fn tex_level_desc(_: *mut c_void, _level: u32, d: *mut u32) -> i32 {
    unsafe {
        *d = 21;
        *d.add(6) = 16;
        *d.add(7) = 8;
    }
    0
}

/// LockRect(level, locked rect, rect, flags): pitch 64, the rect's first byte.
unsafe extern "system" fn tex_lock(this: *mut c_void, _level: u32, lr: *mut D3DLOCKED_RECT, rect: *const i32, _flags: u32) -> i32 {
    let off = if rect.is_null() { 0 } else { unsafe { *rect.add(1) as usize * 64 + *rect as usize * 4 } };
    unsafe {
        (*lr).Pitch = 64;
        (*lr).pBits = (*(this as *mut FakeTex)).bytes.as_mut_ptr().add(off) as *mut c_void;
    }
    0
}

unsafe extern "system" fn tex_unlock(_: *mut c_void, _level: u32) -> i32 {
    0
}

fn tex_call(f: usize, args: &[u32]) -> u32 {
    let m = init_memory();
    let stack = alloc(256);
    let mut c = Cpu::new(m);
    c.r[ESP] = stack + 64;
    for (k, &a) in args.iter().enumerate() {
        m.wr32(stack + 68 + 4 * k as u32, a);
    }
    (TEXTURE[f].2)(&mut c)
}

/// Nested rect locks of different sizes share the level's guest buffer: the outer lock's
/// pointer stays valid and sees the inner lock's bytes at the rect's place.
#[test]
fn texture_rect_locks_share_level() {
    let m = init_memory();
    let mut vt = [0usize; 22];
    vt[17] = tex_level_desc as *const () as usize;
    vt[19] = tex_lock as *const () as usize;
    vt[20] = tex_unlock as *const () as usize;
    let mut fake = Box::new(FakeTex { vtbl: &vt, bytes: [0; 512] });
    for (k, b) in fake.bytes.iter_mut().enumerate() {
        *b = k as u8;
    }
    let host = &mut *fake as *mut FakeTex as *mut c_void;
    let t = com::wrap(TEXTURE, host);
    let (lr, lr2, rect) = (alloc(8), alloc(8), alloc(16));
    // the 2x2 rect at (4, 3) first, then the whole level
    for (k, v) in [4, 3, 6, 5].into_iter().enumerate() {
        m.wr32(rect + 4 * k as u32, v);
    }
    assert_eq!(tex_call(19, &[t, 0, lr, rect, 0]), 0);
    assert_eq!(tex_call(19, &[t, 0, lr2, 0, 0]), 0);
    let (small, whole) = (m.rd32(lr + 4), m.rd32(lr2 + 4));
    assert_eq!(m.rd32(lr), 64);
    assert_eq!(small, whole + 3 * 64 + 16);
    assert_eq!(m.rd8(whole + 3 * 64 + 16), (3 * 64 + 16) as u8);
    m.wr32(small, 0xdead_beef);
    assert_eq!(tex_call(20, &[t, 0]), 0);
    assert_eq!(tex_call(20, &[t, 0]), 0);
    assert_eq!(&fake.bytes[3 * 64 + 16..3 * 64 + 20], &0xdead_beefu32.to_le_bytes());
    assert_eq!(fake.bytes[0], 0);
    forget(t);
    com::release(t, 0);
}
