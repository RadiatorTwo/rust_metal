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
