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
