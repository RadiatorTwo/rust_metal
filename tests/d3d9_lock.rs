//! Direct3D 9 locks between guest buffers and host memory, with a fake host resource (plain
//! host memory). Needs no game files and no GPU.
#![cfg(windows)]

use rust_metal::platform::d3d9::resources::{D3DLOCK_DISCARD, D3DLOCK_READONLY, begin_lock, end_lock, lock_copy_in};
use rust_metal::platform::{alloc, init_memory};

#[test]
fn copy_in_flags() {
    assert!(lock_copy_in(0));
    assert!(lock_copy_in(D3DLOCK_READONLY));
    assert!(!lock_copy_in(D3DLOCK_DISCARD));
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

/// Call a guest method with arguments on a stack in guest memory.
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
    vt[11] = fake_lock as usize;
    vt[12] = fake_unlock as usize;
    vt[13] = fake_get_desc as usize;
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
    com::release(vb, 0);
}
