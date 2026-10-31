//! What GetWriteWatch costs, without the game (tick thread plan, step 0): one reservation of
//! 4 GB with MEM_WRITE_WATCH as the guest memory has, ~160 MB committed as in a mission, some
//! pages written; then calls over ranges of several sizes, with and without reset. For
//! comparison the same over a reservation of only the committed size, and over 16 MB
//! reservations of their own (one call each).
//!
//!     cargo run --release --example watchbench

#[cfg(windows)]
fn main() {
    use std::time::Instant;
    use windows_sys::Win32::System::Memory::{GetWriteWatch, MEM_COMMIT, MEM_RESERVE, PAGE_NOACCESS, PAGE_READWRITE, VirtualAlloc};
    const WW: u32 = 0x20_0000;
    let mut buf = vec![0usize; 1 << 20];
    let mut call = |base: *mut u8, lo: usize, n: usize, reset: bool| -> (f64, usize) {
        let mut count = buf.len();
        let mut gran = 0u32;
        let t = Instant::now();
        unsafe { GetWriteWatch(reset as u32, base.add(lo) as _, n, buf.as_mut_ptr() as _, &mut count, &mut gran) };
        (t.elapsed().as_secs_f64() * 1e3, count)
    };
    let write = |base: *mut u8, lo: usize, pages: usize, stride: usize| {
        for k in 0..pages {
            unsafe { *base.add(lo + k * stride * 4096) = 1 };
        }
    };
    let span = (1usize << 32) + 0x1000;
    let base = unsafe { VirtualAlloc(std::ptr::null(), span, MEM_RESERVE | WW, PAGE_NOACCESS) } as *mut u8;
    assert!(!base.is_null());
    // the guest's layout: stacks, image, heap arena
    let commit = |base: *mut u8, lo: usize, n: usize| assert!(!unsafe { VirtualAlloc(base.add(lo) as _, n, MEM_COMMIT, PAGE_READWRITE) }.is_null());
    commit(base, 0x10_0000, 0x30_0000);
    commit(base, 0x40_0000, 0x37_3000);
    commit(base, 0x2000_0000, 150 << 20);
    let heap = 0x2000_0000usize;
    let avg = |f: &mut dyn FnMut() -> (f64, usize)| {
        let v: Vec<(f64, usize)> = (0..20).map(|_| f()).collect();
        (v.iter().map(|e| e.0).sum::<f64>() / v.len() as f64, v[0].1)
    };
    println!("one 4 GB reservation with write watch, 160 MB committed:");
    for (name, lo, n) in [
        ("4 KB in the heap", heap, 4096usize),
        ("1 MB in the heap", heap, 1 << 20),
        ("16 MB in the heap", heap, 16 << 20),
        ("150 MB heap", heap, 150 << 20),
        ("image 3.5 MB", 0x40_0000, 0x37_3000),
        ("whole reservation", 0, span),
    ] {
        let _ = call(base, 0, span, true);
        let (empty, _) = avg(&mut || call(base, lo, n, true));
        write(base, heap, 800, 2);
        let (full, pages) = call(base, lo, n, true);
        println!("  {name:>20}: {empty:.3} ms without pages, {full:.3} ms with {pages} pages written");
    }
    // a reservation of only 160 MB
    let small = unsafe { VirtualAlloc(std::ptr::null(), 160 << 20, MEM_RESERVE | WW, PAGE_NOACCESS) } as *mut u8;
    commit(small, 0, 160 << 20);
    println!("one 160 MB reservation with write watch, all committed:");
    for (name, n) in [("4 KB", 4096usize), ("16 MB", 16 << 20), ("160 MB", 160 << 20)] {
        let (empty, _) = avg(&mut || call(small, 0, n, true));
        write(small, 0, 800, 2);
        let (full, pages) = call(small, 0, n, true);
        println!("  {name:>20}: {empty:.3} ms without pages, {full:.3} ms with {pages} pages written");
    }
    // ten reservations of 16 MB, each called once
    let parts: Vec<*mut u8> = (0..10)
        .map(|_| {
            let p = unsafe { VirtualAlloc(std::ptr::null(), 16 << 20, MEM_RESERVE | WW, PAGE_NOACCESS) } as *mut u8;
            commit(p, 0, 16 << 20);
            p
        })
        .collect();
    let (empty, _) = avg(&mut || {
        let t = Instant::now();
        for &p in &parts {
            let _ = call(p, 0, 16 << 20, true);
        }
        (t.elapsed().as_secs_f64() * 1e3, 0)
    });
    println!("ten 16 MB reservations, one call each: {empty:.3} ms for all ten without pages");
}

#[cfg(not(windows))]
fn main() {
    eprintln!("GetWriteWatch is Windows only");
}
