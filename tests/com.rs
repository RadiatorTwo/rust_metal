//! Guest COM objects for host interface pointers, with fake host pointers. Needs no game files.

use rust_metal::platform::{ImpFn, Import, com, heap_top, init_memory};
use rust_metal::rt::Cpu;
use std::ffi::c_void;
use std::sync::{Mutex, MutexGuard};

fn nop(_: &mut Cpu) -> u32 {
    0
}

static M: [Import; 2] = [("A", 1, nop as ImpFn), ("B", 1, nop as ImpFn)];

/// The tests share one guest heap: they run one at a time so heap_top means something.
fn setup() -> MutexGuard<'static, ()> {
    static L: Mutex<()> = Mutex::new(());
    init_memory();
    L.lock().unwrap_or_else(|e| e.into_inner())
}

fn fake(n: usize) -> *mut c_void {
    (0x1000 * n) as *mut c_void
}

#[test]
fn same_host_same_guest() {
    let _g = setup();
    let g = com::wrap(&M, fake(1));
    assert_eq!(com::wrap(&M, fake(1)), g);
    assert_eq!(com::host(g), fake(1));
    let h = com::wrap(&M, fake(2));
    assert_ne!(h, g);
    assert_eq!(com::host(h), fake(2));
    com::release(g, 0);
    com::release(h, 0);
}

#[test]
fn no_growth_per_frame() {
    let _g = setup();
    let g = com::wrap(&M, fake(3));
    let top = heap_top();
    for _ in 0..10_000 {
        assert_eq!(com::wrap(&M, fake(3)), g);
        com::release(g, 1);
    }
    assert_eq!(heap_top(), top);
    com::release(g, 0);
}

#[test]
fn release_frees() {
    let _g = setup();
    let g = com::wrap(&M, fake(4));
    let s = com::shadow(g, 0, 4096);
    assert_ne!(s, 0);
    com::release(g, 0);
    let g2 = com::wrap(&M, fake(4));
    assert_eq!(com::host(g2), fake(4));
    // the shadow went back to the heap: the same bytes serve the next asker
    assert_eq!(com::shadow(g2, 0, 4096), s);
    com::release(g2, 0);
}

#[test]
fn shadow_stable() {
    let _g = setup();
    let g = com::wrap(&M, fake(5));
    let a = com::shadow(g, 0, 4096);
    assert_eq!(com::shadow(g, 0, 4096), a);
    assert_eq!(com::shadow(g, 0, 100), a);
    let b = com::shadow(g, 0, 8192);
    assert_eq!(com::shadow(g, 0, 8192), b);
    assert_ne!(com::shadow(g, 1, 16), b);
    com::release(g, 0);
}
