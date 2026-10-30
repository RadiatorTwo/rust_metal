//! DirectInput from the window's input: the keyboard's state by DIK code (extended keys with
//! bit 7), its buffered events (GetDeviceData), the mouse's raw movement, wheel and buttons,
//! and when the cursor is caught. Needs no game files and no window.

use rust_metal::platform::dinput::{self, Buffer, DEVICE, DI_BUFFEROVERFLOW, DIERR_NOTACQUIRED, DIERR_NOTBUFFERED, DIGDD_PEEK, DIPROP_BUFFERSIZE, DIRECT_INPUT7, DISCL_EXCLUSIVE, Event};
use rust_metal::platform::window::{Input, catch_wanted, should_catch, vk_of};
use rust_metal::platform::{Import, alloc, init_memory, user32};
use rust_metal::rt::{Cpu, ESP};
use std::sync::{Mutex, MutexGuard};
use winit::keyboard::KeyCode;

/// The tests share the window's key state and the devices: one at a time.
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

const ACQUIRE: usize = 7;
const UNACQUIRE: usize = 8;
const GET_DEVICE_STATE: usize = 9;
const GET_DEVICE_DATA: usize = 10;
const SET_PROPERTY: usize = 6;
const GET_PROPERTY: usize = 5;
const SET_COOPERATIVE_LEVEL: usize = 13;
const RELEASE: usize = 2;

/// A device of the game's kind through IDirectInput7A::CreateDevice.
fn device(g: [u8; 16]) -> u32 {
    let m = init_memory();
    let (iid, out) = (alloc(16), alloc(4));
    m.slice(iid, 16).copy_from_slice(&[0x85, 0xb6, 0x4c, 0x9a, 0x6d, 0x23, 0xd3, 0x11, 0x8e, 0x9d, 0x00, 0xc0, 0x4f, 0x68, 0x44, 0xae]);
    assert_eq!(dinput::create(iid, out), 0);
    let di = m.rd32(out);
    let gp = alloc(16);
    m.slice(gp, 16).copy_from_slice(&g);
    assert_eq!(call(DIRECT_INPUT7, 3, &[di, gp, out, 0]), 0);
    m.rd32(out)
}

/// SetProperty(DIPROP_BUFFERSIZE, n) as kb_set_buffer (0x4dcc60): a DIPROPDWORD.
fn set_buffer_size(d: u32, n: u32) {
    let m = init_memory();
    let p = alloc(20);
    for (k, v) in [20, 16, 0, 0, n].into_iter().enumerate() {
        m.wr32(p + 4 * k as u32, v);
    }
    assert_eq!(call(DEVICE, SET_PROPERTY, &[d, DIPROP_BUFFERSIZE, p]), 0);
    m.wr32(p + 16, 0);
    assert_eq!(call(DEVICE, GET_PROPERTY, &[d, DIPROP_BUFFERSIZE, p]), 0);
    assert_eq!(m.rd32(p + 16), n);
}

fn key(code: KeyCode, down: bool) {
    let (vk, scan) = vk_of(code).unwrap();
    user32::key_input(vk, scan, down);
}

/// GetDeviceData into `n` 16-byte DIDEVICEOBJECTDATA: result and (ofs, data) of each.
fn read(d: u32, n: u32, flags: u32) -> (u32, Vec<(u32, u32)>, Vec<Event>) {
    let m = init_memory();
    let (data, count) = (alloc(16 * n.max(1)), alloc(4));
    m.wr32(count, n);
    let r = call(DEVICE, GET_DEVICE_DATA, &[d, 16, data, count, flags]);
    let got = m.rd32(count);
    let ev: Vec<Event> = (0..got).map(|k| Event { ofs: m.rd32(data + 16 * k), data: m.rd32(data + 16 * k + 4), time: m.rd32(data + 16 * k + 8), seq: m.rd32(data + 16 * k + 12) }).collect();
    (r, ev.iter().map(|e| (e.ofs, e.data)).collect(), ev)
}

#[test]
fn modifiers_and_extended_keys() {
    let _g = setup();
    let m = init_memory();
    let out = alloc(256);
    for (code, dik) in [
        (KeyCode::ShiftLeft, 0x2a),
        (KeyCode::ShiftRight, 0x36),
        (KeyCode::ControlLeft, 0x1d),
        (KeyCode::ControlRight, 0x9d),
        (KeyCode::AltLeft, 0x38),
        (KeyCode::AltRight, 0xb8),
        (KeyCode::ArrowUp, 0xc8),
        (KeyCode::ArrowLeft, 0xcb),
        (KeyCode::Numpad8, 0x48),
        (KeyCode::KeyA, 0x1e),
    ] {
        key(code, true);
        dinput::keyboard_state(out, 256);
        let down: Vec<usize> = (0..256).filter(|&k| m.rd8(out + k as u32) == 0x80).collect();
        assert_eq!(down, vec![dik], "{code:?}");
        key(code, false);
        dinput::keyboard_state(out, 256);
        assert!((0..256).all(|k| m.rd8(out + k) == 0), "{code:?} released");
    }
}

#[test]
fn buffer_keeps_the_oldest_and_reports_overflow_once() {
    let e = |n| Event { ofs: n, data: 0x80, time: 0, seq: n };
    let mut b = Buffer::new(2);
    for n in 1..=3 {
        b.push(e(n));
    }
    assert_eq!(b.take(10, true), (vec![e(1), e(2)], true), "peek");
    assert_eq!(b.take(1, false), (vec![e(1)], true));
    assert_eq!(b.take(10, false), (vec![e(2)], false), "overflow said once");
    b.push(e(4));
    assert_eq!(b.take(10, false), (vec![e(4)], false));
    let mut none = Buffer::new(0);
    none.push(e(5));
    assert_eq!(none.take(10, false), (vec![], false), "no buffer: nothing kept");
}

#[test]
fn keyboard_buffers_taps_between_reads() {
    let _g = setup();
    let kb = device(dinput::GUID_SYSKEYBOARD);
    set_buffer_size(kb, 3);
    // not acquired: no data, no state (the game re-Acquires then)
    assert_eq!(read(kb, 10, 0).0, DIERR_NOTACQUIRED);
    assert_eq!(call(DEVICE, GET_DEVICE_STATE, &[kb, 256, alloc(256)]), DIERR_NOTACQUIRED);
    key(KeyCode::KeyB, true);
    key(KeyCode::KeyB, false);
    assert_eq!(call(DEVICE, ACQUIRE, &[kb]), 0);
    assert_eq!(call(DEVICE, ACQUIRE, &[kb]), 1, "already acquired: S_FALSE");
    assert_eq!(read(kb, 10, 0), (0, vec![], vec![]), "keys before Acquire are not buffered");

    // a tap: press and release before the game reads; a key repeat is no new event
    key(KeyCode::KeyW, true);
    key(KeyCode::KeyW, true);
    key(KeyCode::KeyW, false);
    let (r, peeked, _) = read(kb, 10, DIGDD_PEEK);
    assert_eq!((r, peeked), (0, vec![(0x11, 0x80), (0x11, 0)]));
    let (r, got, ev) = read(kb, 1, 0);
    assert_eq!((r, got), (0, vec![(0x11, 0x80)]), "one asked for, one taken");
    let (_, got, ev2) = read(kb, 10, 0);
    assert_eq!(got, vec![(0x11, 0)]);
    assert!(ev2[0].seq > ev[0].seq && ev2[0].time >= ev[0].time, "sequence and time go up");
    assert_eq!(read(kb, 10, 0).1, vec![]);

    // more than DIPROP_BUFFERSIZE events: the first three stay, overflow once
    key(KeyCode::ArrowUp, true);
    key(KeyCode::ArrowUp, false);
    key(KeyCode::Space, true);
    key(KeyCode::Space, false);
    let (r, got, _) = read(kb, 10, 0);
    assert_eq!((r, got), (DI_BUFFEROVERFLOW, vec![(0xc8, 0x80), (0xc8, 0), (0x39, 0x80)]));
    assert_eq!(read(kb, 10, 0).0, 0);

    // data 0: counted (peek) or dropped
    let m = init_memory();
    let count = alloc(4);
    key(KeyCode::KeyA, true);
    m.wr32(count, u32::MAX);
    assert_eq!(call(DEVICE, GET_DEVICE_DATA, &[kb, 16, 0, count, DIGDD_PEEK]), 0);
    assert_eq!(m.rd32(count), 1);
    key(KeyCode::KeyA, false);
    m.wr32(count, u32::MAX);
    assert_eq!(call(DEVICE, GET_DEVICE_DATA, &[kb, 16, 0, count, 0]), 0, "flush");
    assert_eq!(m.rd32(count), 2);
    assert_eq!(read(kb, 10, 0).1, vec![]);

    // DirectInput 8's 20-byte DIDEVICEOBJECTDATA: the same, uAppData 0
    key(KeyCode::KeyD, true);
    let (data, count) = (alloc(40), alloc(4));
    m.slice(data, 40).fill(0xee);
    m.wr32(count, 2);
    assert_eq!(call(DEVICE, GET_DEVICE_DATA, &[kb, 20, data, count, 0]), 0);
    assert_eq!((m.rd32(count), m.rd32(data), m.rd32(data + 4), m.rd32(data + 16)), (1, 0x20, 0x80, 0));
    assert_eq!(m.rd32(data + 20), 0xeeee_eeee, "the second slot untouched");
    key(KeyCode::KeyD, false);

    assert_eq!(call(DEVICE, UNACQUIRE, &[kb]), 0);
    assert_eq!(read(kb, 10, 0).0, DIERR_NOTACQUIRED);
    assert_eq!(call(DEVICE, RELEASE, &[kb]), 0);
}

#[test]
fn unbuffered_device_says_so() {
    let _g = setup();
    let kb = device(dinput::GUID_SYSKEYBOARD);
    assert_eq!(call(DEVICE, ACQUIRE, &[kb]), 0);
    assert_eq!(read(kb, 10, 0).0, DIERR_NOTBUFFERED);
    assert_eq!(call(DEVICE, RELEASE, &[kb]), 0);
}

/// GetDeviceState of the mouse, c_dfDIMouse2: x, y, wheel, buttons.
fn mouse(d: u32) -> (i32, i32, i32, [u8; 8]) {
    let m = init_memory();
    let out = alloc(20);
    assert_eq!(call(DEVICE, GET_DEVICE_STATE, &[d, 20, out]), 0);
    let mut b = [0; 8];
    b.copy_from_slice(m.slice(out + 12, 8));
    (m.rd32(out) as i32, m.rd32(out + 4) as i32, m.rd32(out + 8) as i32, b)
}

#[test]
fn mouse_moves_by_raw_motion_and_scripted_moves() {
    let _g = setup();
    let ms = device(dinput::GUID_SYSMOUSE);
    assert_eq!(call(DEVICE, SET_COOPERATIVE_LEVEL, &[ms, 0, DISCL_EXCLUSIVE | 4]), 0);
    user32::feed(Input::Motion(500, 0), false);
    assert_eq!(call(DEVICE, ACQUIRE, &[ms]), 0);
    assert!(catch_wanted(), "an exclusive mouse catches the cursor");
    assert_eq!(mouse(ms).0, 0, "movement before Acquire does not count");

    // a real run: raw movement adds up, cursor positions do not count (they stop at the edge)
    user32::feed(Input::Motion(30, -4), false);
    user32::feed(Input::Motion(-10, 2), false);
    user32::feed(Input::Move(600, 400), false);
    user32::feed(Input::Wheel(2), false);
    user32::feed(Input::Wheel(-1), false);
    user32::feed(Input::Button(0, true), false);
    user32::feed(Input::Button(4, true), false);
    assert_eq!(mouse(ms), (20, -2, 120, [0x80, 0, 0, 0, 0x80, 0, 0, 0]));
    assert_eq!(mouse(ms), (0, 0, 0, [0x80, 0, 0, 0, 0x80, 0, 0, 0]), "movement since the last read; held buttons stay");
    user32::feed(Input::Button(0, false), false);
    user32::feed(Input::Button(4, false), false);
    assert_eq!(mouse(ms).3, [0; 8]);

    // a scripted run: moves are the movement (from the last position)
    user32::feed(Input::Move(100, 100), true);
    mouse(ms);
    user32::feed(Input::Move(130, 90), true);
    let (x, y, ..) = mouse(ms);
    assert_eq!((x, y), (30, -10));

    // focus lost: what was not read is dropped
    user32::feed(Input::Motion(7, 7), false);
    user32::feed(Input::Focus(false), false);
    assert_eq!(mouse(ms).0, 0);

    assert_eq!(call(DEVICE, UNACQUIRE, &[ms]), 0);
    assert!(!catch_wanted(), "unacquired: the cursor is free");
    assert_eq!(call(DEVICE, ACQUIRE, &[ms]), 0);
    assert!(catch_wanted());
    assert_eq!(call(DEVICE, RELEASE, &[ms]), 0);
    assert!(!catch_wanted(), "released: the cursor is free");
}

#[test]
fn non_exclusive_mouse_leaves_the_cursor() {
    let _g = setup();
    let ms = device(dinput::GUID_SYSMOUSE);
    assert_eq!(call(DEVICE, SET_COOPERATIVE_LEVEL, &[ms, 0, 2 | 4]), 0);
    assert_eq!(call(DEVICE, ACQUIRE, &[ms]), 0);
    assert!(!catch_wanted());
    assert_eq!(call(DEVICE, RELEASE, &[ms]), 0);
}

#[test]
fn cursor_caught_only_for_an_acquired_mouse_in_front_and_unscripted() {
    for acquired in [false, true] {
        for focused in [false, true] {
            for scripted in [false, true] {
                assert_eq!(should_catch(acquired, focused, scripted), acquired && focused && !scripted);
            }
        }
    }
}
