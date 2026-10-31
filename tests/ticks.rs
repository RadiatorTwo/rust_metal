//! The tick aids of the tick thread's step 0: the logic fingerprint's line format, finding the
//! first difference of two runs, and tick-indexed `GUNMETAL_INPUT` items. Needs no game files.

use rust_metal::platform::ticks::{Print, first_divergence, fnv, print_line};
use rust_metal::platform::window::{Input, parse_script};

fn sample() -> Print {
    let mut camera = [0.0f32; 12];
    camera[0] = 1.5;
    camera[1] = -0.1;
    camera[3] = 1.0;
    Print { tick: 7, time: 280, camera, player: Some((3, 0x0abc_0123)), objects: vec![(2, 14), (5, 3)], units: 0xdead_beef, rand: (4, 0x1234, 0x99) }
}

#[test]
fn fingerprint_line_has_every_field_in_order() {
    let l = print_line(&sample());
    let keys: Vec<&str> = l.split_whitespace().map(|f| f.split('=').next().unwrap()).collect();
    assert_eq!(keys, ["tick", "time", "cam", "player", "objects", "units", "rand"]);
    assert!(l.starts_with("tick=7 time=280 cam=1.5,-0.1,0.0,1.0,"), "{l}");
    assert!(l.contains(" player=3:0abc0123 "), "{l}");
    assert!(l.ends_with(" objects=2:14,5:3 units=deadbeef rand=4:00001234:00000099"), "{l}");
    // no player, no containers
    let p = Print { player: None, objects: vec![], ..sample() };
    let l = print_line(&p);
    assert!(l.contains(" player=- objects=- "), "{l}");
}

#[test]
fn fingerprint_floats_are_exact() {
    // equal text means equal bits: the shortest form that reads back as the same f32
    let x = f32::from_bits(0x3dcc_cccd);
    let mut p = sample();
    p.camera[0] = x;
    let l = print_line(&p);
    let v: f32 = l.split("cam=").nth(1).unwrap().split(',').next().unwrap().parse().unwrap();
    assert_eq!(v.to_bits(), x.to_bits());
    p.camera[0] = f32::from_bits(0x3dcc_ccce);
    assert_ne!(print_line(&p), l);
}

#[test]
fn first_divergence_names_line_and_field() {
    let a = print_line(&sample());
    let mut q = sample();
    q.rand = (4, 0x1235, 0x99);
    let b = print_line(&q);
    assert_eq!(first_divergence(&[&a, &a], &[&a, &a]), None);
    assert_eq!(first_divergence(&[&a, &a], &[&a, &b]), Some((1, "rand".into())));
    q.time = 320;
    let c = print_line(&q);
    assert_eq!(first_divergence(&[&a], &[&c]), Some((0, "time".into())));
    // one run is shorter
    assert_eq!(first_divergence(&[&a, &a], &[&a]), Some((1, "tick".into())));
}

#[test]
fn fnv_depends_on_order() {
    let h = 0x811c_9dc5;
    assert_ne!(fnv(fnv(h, 1), 2), fnv(fnv(h, 2), 1));
    assert_eq!(fnv(h, 0), fnv(h, 0));
}

#[test]
fn tick_items_go_to_the_tick_queue() {
    let s = parse_script("16000:Enter t10:W*25 t12:click@512,384 t40:move@600,384 t50:close 50000:move@512,384");
    let ms: Vec<u64> = s.ms.iter().map(|e| e.0).collect();
    // Enter down and up (100 ms), the move
    assert_eq!(ms, [16000, 16100, 50000]);
    let ticks: Vec<(u64, Input)> = s.ticks.iter().copied().collect();
    let at: Vec<u64> = ticks.iter().map(|e| e.0).collect();
    // W down at 10 and up 25 ticks later; the click: move at 12, down 13, up 15
    assert_eq!(at, [10, 12, 13, 15, 35, 40, 50]);
    assert!(matches!(ticks[0].1, Input::Key { vk: 0x57, down: true, .. }));
    assert!(matches!(ticks[1].1, Input::Move(512, 384)));
    assert!(matches!(ticks[2].1, Input::Button(0, true)));
    assert!(matches!(ticks[3].1, Input::Button(0, false)));
    assert!(matches!(ticks[4].1, Input::Key { vk: 0x57, down: false, .. }));
    assert!(matches!(ticks[6].1, Input::Close));
}

#[test]
fn tick_keys_hold_three_ticks_by_default() {
    let s = parse_script("t5:Space t5:text@ab");
    let at: Vec<u64> = s.ticks.iter().map(|e| e.0).collect();
    // stable order at the same tick: the key, then the first character
    assert_eq!(at, [5, 5, 6, 8]);
    assert!(matches!(s.ticks[1].1, Input::Char(b'a')));
    assert!(matches!(s.ticks[3].1, Input::Key { down: false, .. }));
    assert!(s.ms.is_empty());
    // a bad tick number is skipped
    assert!(parse_script("tx:W").ticks.is_empty());
    // a stall, in ticks and in ms
    let s = parse_script("t9:stall*120 3000:stall*50");
    assert!(matches!(s.ticks[0], (9, Input::Stall(120))));
    assert!(matches!(s.ms[0], (3000, Input::Stall(50))));
}
