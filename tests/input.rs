//! Scripted runs ignore the real keyboard, mouse and focus (the script has `blur`, `focus`,
//! `minimize`, `restore`); the window's close button still counts. Needs no game files and no window.

use rust_metal::platform::window::{Input, reaches_game};

#[test]
fn scripted_runs_ignore_real_keyboard_and_mouse() {
    let real = [
        Input::Key { vk: 0x0d, scan: 0x1c, down: true, repeat: false },
        Input::Char(b'a'),
        Input::Move(10, 20),
        Input::Motion(3, -4),
        Input::Button(0, true),
        Input::Wheel(1),
        Input::Focus(false),
        Input::Focus(true),
        Input::Minimized(true),
    ];
    for i in &real {
        assert!(!reaches_game(i, true), "{i:?} reached a scripted run");
        assert!(reaches_game(i, false), "{i:?} did not reach a normal run");
    }
    assert!(reaches_game(&Input::Close, true) && reaches_game(&Input::Close, false));
}
