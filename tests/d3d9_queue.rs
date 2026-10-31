//! The Direct3D 9 command queue (`d3d9::queue`): order, waiting, one picture ahead. Plain
//! closures, no device and no game files.
#![cfg(windows)]

use rust_metal::platform::d3d9::queue::Queue;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[test]
fn runs_in_record_order() {
    let q = Queue::new();
    let seen = Arc::new(Mutex::new(Vec::new()));
    for i in 0..1000u32 {
        let s = seen.clone();
        q.record(Box::new(move || s.lock().unwrap().push(i)));
    }
    q.flush();
    assert_eq!(*seen.lock().unwrap(), (0..1000).collect::<Vec<_>>());
    assert_eq!(q.pending(), 0);
}

#[test]
fn flush_waits() {
    let q = Queue::new();
    let done = Arc::new(AtomicBool::new(false));
    let d = done.clone();
    q.record(Box::new(move || {
        std::thread::sleep(Duration::from_millis(50));
        d.store(true, Ordering::SeqCst);
    }));
    q.flush();
    assert!(done.load(Ordering::SeqCst));
}

#[test]
fn one_picture_ahead() {
    let q = Queue::new();
    let t = Instant::now();
    q.record(Box::new(|| std::thread::sleep(Duration::from_millis(100))));
    q.picture_done();
    // no picture before it: back at once
    assert!(t.elapsed() < Duration::from_millis(50), "{:?}", t.elapsed());
    q.record(Box::new(|| std::thread::sleep(Duration::from_millis(100))));
    q.picture_done();
    // waited for the first picture, not for the second
    let e = t.elapsed();
    assert!(e >= Duration::from_millis(90) && e < Duration::from_millis(190), "{e:?}");
    q.flush();
}

#[test]
fn two_threads_record_in_order() {
    let q = Arc::new(Queue::new());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let turn = Arc::new(Mutex::new(()));
    let threads: Vec<_> = (0..2u32)
        .map(|th| {
            let (q, seen, turn) = (q.clone(), seen.clone(), turn.clone());
            std::thread::spawn(move || {
                for i in 0..500u32 {
                    let _g = turn.lock().unwrap();
                    let s = seen.clone();
                    q.record(Box::new(move || s.lock().unwrap().push((th, i))));
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    q.flush();
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1000);
    for th in 0..2 {
        let mine: Vec<u32> = seen.iter().filter(|e| e.0 == th).map(|e| e.1).collect();
        assert_eq!(mine, (0..500).collect::<Vec<_>>());
    }
}

#[test]
fn queued_names() {
    use rust_metal::platform::d3d9::queue::queued_method;
    assert!(queued_method("IDirect3DDevice9::DrawIndexedPrimitive"));
    assert!(queued_method("IDirect3DIndexBuffer9::Unlock"));
    assert!(!queued_method("IDirect3DDevice9::CreateTexture"));
    assert!(!queued_method("IDirect3DDevice9::GetTransform"));
    assert!(!queued_method("IDirect3DTexture9::LockRect"));
}

/// The order a Release keeps: flush, then the host's Release; a recorded use of the object is
/// made before.
#[test]
fn release_waits_for_recorded_use() {
    let q = Queue::new();
    let used = Arc::new(AtomicBool::new(false));
    let u = used.clone();
    q.record(Box::new(move || {
        std::thread::sleep(Duration::from_millis(50));
        u.store(true, Ordering::SeqCst);
    }));
    q.flush();
    // the host's Release would run here
    assert!(used.load(Ordering::SeqCst));
}

/// A recorded call's data is a copy taken when it is recorded: the game may change its bytes
/// right after the call returns.
#[test]
fn copy_in_takes_a_snapshot() {
    use rust_metal::platform::d3d9::device::copy_in;
    use rust_metal::platform::{alloc, init_memory};
    let m = init_memory();
    let a = alloc(64);
    for k in 0..64u32 {
        m.slice(a + k, 1)[0] = k as u8 + 1;
    }
    let copy = copy_in(a, 64).unwrap();
    m.slice(a, 64).fill(0);
    assert_eq!(&copy[..], &(1..=64u8).collect::<Vec<_>>()[..]);
    assert!(copy_in(0, 64).is_none());
}
