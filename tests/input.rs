//! Scripted runs ignore the real keyboard and mouse; the window's close button and focus still
//! count. Needs no game files and no window.

use rust_metal::platform::window::{Input, reaches_game};

#[test]
fn scripted_runs_ignore_real_keyboard_and_mouse() {
    let real = [
        Input::Key { vk: 0x0d, scan: 0x1c, down: true, repeat: false },
        Input::Char(b'a'),
        Input::Move(10, 20),
        Input::Button(0, true),
        Input::Wheel(1),
    ];
    for i in &real {
        assert!(!reaches_game(i, true), "{i:?} reached a scripted run");
        assert!(reaches_game(i, false), "{i:?} did not reach a normal run");
    }
    for i in [Input::Close, Input::Focus(false), Input::Focus(true)] {
        assert!(reaches_game(&i, true) && reaches_game(&i, false), "{i:?}");
    }
}
