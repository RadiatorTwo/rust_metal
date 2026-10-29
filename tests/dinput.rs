//! DirectInput's keyboard state from the window's key events: DIK codes from the scan codes,
//! extended keys (right Ctrl and Alt, arrows) with bit 7. Needs no game files.

use rust_metal::platform::window::vk_of;
use rust_metal::platform::{alloc, dinput, init_memory, user32};
use winit::keyboard::KeyCode;

#[test]
fn modifiers_and_extended_keys() {
    let m = init_memory();
    let out = alloc(256);
    for (key, dik) in [
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
        let (vk, scan) = vk_of(key).unwrap();
        user32::key_input(vk, scan, true);
        dinput::keyboard_state(out, 256);
        let down: Vec<usize> = (0..256).filter(|&k| m.rd8(out + k as u32) == 0x80).collect();
        assert_eq!(down, vec![dik], "{key:?}");
        user32::key_input(vk, scan, false);
        dinput::keyboard_state(out, 256);
        assert!((0..256).all(|k| m.rd8(out + k) == 0), "{key:?} released");
    }
}
