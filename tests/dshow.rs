//! DirectShow's filter graph and DirectInput's creation, through their guest methods. Needs no
//! game files.

use rust_metal::platform::dshow::{self, BASIC_AUDIO, E_ABORT, EC_COMPLETE, ENUM_PINS, Events, FILTER, GRAPH_BUILDER, MEDIA_CONTROL, MEDIA_EVENT, MEDIA_POSITION, S_FALSE, S_OK};
use rust_metal::platform::{Import, alloc, dinput, heap_top, init_memory, kernel32};
use rust_metal::rt::{Cpu, ESP};
use std::sync::{Mutex, MutexGuard};

/// The tests share one guest heap: they run one at a time so heap_top means something.
fn setup() -> MutexGuard<'static, ()> {
    static L: Mutex<()> = Mutex::new(());
    init_memory();
    L.lock().unwrap_or_else(|e| e.into_inner())
}

/// Call method `k` of a method table with arguments on a stack in guest memory.
fn call(table: &[Import], k: usize, args: &[u32]) -> u32 {
    let m = init_memory();
    let stack = alloc(256);
    let mut c = Cpu::new(m);
    c.r[ESP] = stack + 64;
    for (n, &a) in args.iter().enumerate() {
        m.wr32(stack + 68 + 4 * n as u32, a);
    }
    let r = (table[k].2)(&mut c);
    rust_metal::platform::free(stack);
    r
}

/// A GUID in guest memory.
fn guest_guid(g: [u8; 16]) -> u32 {
    let p = alloc(16);
    init_memory().slice(p, 16).copy_from_slice(&g);
    p
}

fn query(table: &[Import], this: u32, iid: [u8; 16]) -> u32 {
    let (p, out) = (guest_guid(iid), alloc(4));
    assert_eq!(call(table, 0, &[this, p, out]), S_OK);
    init_memory().rd32(out)
}

fn new_graph() -> u32 {
    let (iid, out) = (guest_guid(dshow::IID_IGRAPHBUILDER), alloc(4));
    assert_eq!(dshow::create_graph(iid, out), S_OK);
    init_memory().rd32(out)
}

#[test]
fn events_in_order_and_handle() {
    let _g = setup();
    let mut e = Events::default();
    let h = e.handle();
    assert_eq!(kernel32::wait(&[h], false, 0), 0x102);
    e.post(EC_COMPLETE, 0, 0);
    e.post(2, 3, 4);
    assert_eq!(kernel32::wait(&[h], false, 0), 0);
    assert_eq!(e.get(), Some([EC_COMPLETE, 0, 0]));
    // still one waiting: still set
    assert_eq!(kernel32::wait(&[h], false, 0), 0);
    assert_eq!(e.get(), Some([2, 3, 4]));
    assert_eq!(e.get(), None);
    assert_eq!(kernel32::wait(&[h], false, 0), 0x102);
    assert_eq!(e.handle(), h);
}

/// What the game's video players see: Run ends the video at once.
#[test]
fn run_ends_at_once() {
    let _g = setup();
    let m = init_memory();
    let gb = new_graph();
    let mc = query(GRAPH_BUILDER, gb, dshow::IID_IMEDIACONTROL);
    let me = query(GRAPH_BUILDER, gb, dshow::IID_IMEDIAEVENT);
    let mp = query(MEDIA_CONTROL, mc, dshow::IID_IMEDIAPOSITION);
    let ba = query(MEDIA_EVENT, me, dshow::IID_IBASICAUDIO);
    // the same object behind every interface
    assert_eq!(query(MEDIA_POSITION, mp, dshow::IID_IGRAPHBUILDER), gb);
    call(GRAPH_BUILDER, 2, &[gb]);
    let file: Vec<u16> = "data\\frontend\\intro1_ps_h.wmv\0".encode_utf16().collect();
    let wfile = alloc(2 * file.len() as u32);
    for (k, u) in file.iter().enumerate() {
        m.wr16(wfile + 2 * k as u32, *u);
    }
    // AddSourceFilter: a filter without pins
    let src_out = alloc(4);
    assert_eq!(call(GRAPH_BUILDER, 14, &[gb, wfile, 0, src_out]), S_OK);
    let src = m.rd32(src_out);
    let enum_out = alloc(4);
    assert_eq!(call(FILTER, 10, &[src, enum_out]), S_OK);
    let en = m.rd32(enum_out);
    let (pin, fetched) = (alloc(4), alloc(4));
    m.wr32(fetched, 7);
    assert_eq!(call(ENUM_PINS, 3, &[en, 1, pin, fetched]), S_FALSE);
    assert_eq!(m.rd32(fetched), 0);
    assert_eq!(call(ENUM_PINS, 2, &[en]), 0);
    // the graph keeps the source
    assert_eq!(call(FILTER, 2, &[src]), 1);
    // no event before Run
    let (code, p1, p2) = (alloc(4), alloc(4), alloc(4));
    assert_eq!(call(MEDIA_EVENT, 8, &[me, code, p1, p2, 0]), E_ABORT);
    assert_eq!(call(MEDIA_CONTROL, 7, &[mc]), S_OK);
    assert_eq!(call(MEDIA_EVENT, 8, &[me, code, p1, p2, 0]), S_OK);
    assert_eq!(m.rd32(code), EC_COMPLETE);
    assert_eq!(call(MEDIA_EVENT, 8, &[me, code, p1, p2, 0]), E_ABORT);
    // at the end: duration = position
    let (d, p) = (alloc(8), alloc(8));
    m.slice(d, 8).fill(0xff);
    m.slice(p, 8).fill(0xff);
    call(MEDIA_POSITION, 7, &[mp, d]);
    call(MEDIA_POSITION, 9, &[mp, p]);
    let f = |a| f64::from_le_bytes(m.slice(a, 8).try_into().unwrap());
    assert!(f(d) - f(p) < 0.001);
    // volume kept
    assert_eq!(call(BASIC_AUDIO, 7, &[ba, (-1000i32) as u32]), S_OK);
    let v = alloc(4);
    call(BASIC_AUDIO, 8, &[ba, v]);
    assert_eq!(m.rd32(v) as i32, -1000);
    // the last Release ends the graph and the source with it
    for (t, i) in [(BASIC_AUDIO, ba), (MEDIA_POSITION, mp), (MEDIA_EVENT, me), (MEDIA_CONTROL, mc)] {
        assert_ne!(call(t, 2, &[i]), 0);
    }
    assert_eq!(call(GRAPH_BUILDER, 2, &[gb]), 0);
}

#[test]
fn graphs_do_not_leak() {
    let _g = setup();
    let one = || {
        let gb = new_graph();
        let mc = query(GRAPH_BUILDER, gb, dshow::IID_IMEDIACONTROL);
        call(MEDIA_CONTROL, 7, &[mc]);
        call(MEDIA_CONTROL, 2, &[mc]);
        assert_eq!(call(GRAPH_BUILDER, 2, &[gb]), 0);
    };
    // vtables and argument blocks are made at first use
    one();
    let iid = guest_guid(dshow::IID_IGRAPHBUILDER);
    let out = alloc(4);
    let top = heap_top();
    for _ in 0..100 {
        assert_eq!(dshow::create_graph(iid, out), S_OK);
        let gb = init_memory().rd32(out);
        assert_eq!(call(GRAPH_BUILDER, 2, &[gb]), 0);
    }
    assert_eq!(heap_top(), top);
}

#[test]
fn unknown_interface_refused() {
    let _g = setup();
    let (iid, out) = (guest_guid([7; 16]), alloc(4));
    assert_ne!(dshow::create_graph(iid, out), S_OK);
    assert_eq!(init_memory().rd32(out), 0);
}

#[test]
fn audio_files() {
    assert!(dshow::is_audio("data\\Music\\Track01.WMA"));
    assert!(dshow::is_audio("x.wav"));
    assert!(!dshow::is_audio("data\\frontend\\intro1_ps_h.wmv"));
}

#[test]
fn direct_input_as_the_game_makes_it() {
    let _g = setup();
    let di7w = rust_metal::platform::ole32::guid(0x9a4c_b685, 0x236d, 0x11d3, [0x8e, 0x9d, 0x00, 0xc0, 0x4f, 0x68, 0x44, 0xae]);
    let di7a = rust_metal::platform::ole32::guid(0x9a4c_b684, 0x236d, 0x11d3, [0x8e, 0x9d, 0x00, 0xc0, 0x4f, 0x68, 0x44, 0xae]);
    assert!(dinput::is_direct_input_iid(&di7w) && dinput::is_direct_input_iid(&di7a));
    let (iid, out) = (guest_guid(di7w), alloc(4));
    assert_eq!(dinput::create(iid, out), 0);
    assert_ne!(init_memory().rd32(out), 0);
    let (bad, out2) = (guest_guid([9; 16]), alloc(4));
    assert_ne!(dinput::create(bad, out2), 0);
    assert_eq!(init_memory().rd32(out2), 0);
}
