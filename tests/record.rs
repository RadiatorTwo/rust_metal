//! `GUNMETAL_RECORD`'s items and their replay (`platform::record`, `window::parse_script`).

use rust_metal::platform::record::{At, Recorder, key_name};
use rust_metal::platform::window::{Input, parse_script};

fn key(vk: u8, down: bool) -> Input {
    Input::Key { vk, scan: 0, down, repeat: false }
}

#[test]
fn keys_buttons_and_motion_become_items() {
    let mut r = Recorder::default();
    let mut lines = Vec::new();
    lines.extend(r.event(&key(0x0d, true), At::Ms(16000)));
    lines.extend(r.event(&key(0x0d, true), At::Ms(16050))); // held: no second press
    lines.extend(r.event(&key(0x0d, false), At::Ms(16120)));
    lines.extend(r.event(&Input::Motion(3, -1), At::Tick(10)));
    lines.extend(r.event(&Input::Motion(2, 1), At::Tick(10)));
    lines.extend(r.event(&key(b'W', true), At::Tick(11)));
    lines.extend(r.event(&Input::Button(0, true), At::Tick(12)));
    lines.extend(r.event(&Input::Button(0, false), At::Tick(20)));
    lines.extend(r.event(&key(b'W', false), At::Tick(40)));
    lines.extend(r.event(&Input::Close, At::Tick(41)));
    assert_eq!(lines, ["16000:Enter*120", "t10:motion@5,0", "t12:LButton*8", "t11:W*29", "t41:close"]);
}

#[test]
fn recorded_items_play_back() {
    let s = parse_script("16000:Enter*120 # a comment\nt10:motion@5,0\nt11:W*29\nt12:LButton*8\nt13:wheel@-1");
    assert_eq!(s.ms.len(), 2);
    let ticks: Vec<String> = s.ticks.iter().map(|(t, i)| format!("{t} {i:?}")).collect();
    assert_eq!(ticks[0], "10 Motion(5, 0)");
    assert!(ticks[1].starts_with("11 Key { vk: 87") && ticks[1].contains("down: true"));
    assert_eq!(ticks[2], "12 Button(0, true)");
    assert_eq!(ticks[3], "13 Wheel(-1)");
    assert_eq!(ticks[4], "20 Button(0, false)");
    assert!(ticks[5].starts_with("40 Key { vk: 87") && ticks[5].contains("down: false"));
}

#[test]
fn key_names_read_back() {
    for vk in [b'A', b'7', 0x0d, 0x1b, 0x20, 0x26, 0x11, 0x12, 0x10, 0x70, 0x7b, 0x60, 0x08] {
        let name = key_name(vk).unwrap();
        let s = parse_script(&format!("0:{name}"));
        match s.ms.front() {
            Some((_, Input::Key { vk: v, .. })) => assert_eq!(*v, vk, "{name}"),
            other => panic!("{name}: {other:?}"),
        }
    }
}
