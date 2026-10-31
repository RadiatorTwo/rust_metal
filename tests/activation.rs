//! The game's window hears when the real window loses or gets the focus and when it is
//! minimised (WM_ACTIVATEAPP and friends, WM_SIZE), and TestCooperativeLevel's answer during a
//! simulated device loss. Needs no game files and no window.

use rust_metal::platform::d3d9::device::{D3DERR_DEVICELOST, D3DERR_DEVICENOTRESET, cooperative_level};
use rust_metal::platform::user32::{self, Msg, SIZE_MINIMIZED, SIZE_RESTORED, WM_ACTIVATE, WM_ACTIVATEAPP, WM_KILLFOCUS, WM_SETFOCUS, WM_SIZE};
use rust_metal::platform::window::Input;
use std::time::{Duration, Instant};

fn messages() -> Vec<(u32, u32, u32)> {
    user32::take_queued().iter().map(|m: &Msg| (m.msg, m.wp, m.lp)).collect()
}

#[test]
fn focus_and_minimise_send_what_windows_sends() {
    let hwnd = user32::test_main_window(1024, 768);
    assert_eq!(user32::main_window(), hwnd);

    user32::feed(Input::Focus(true), false);
    assert_eq!(messages(), [], "shown is active already");

    user32::feed(Input::Focus(false), false);
    assert_eq!(messages(), [(WM_ACTIVATE, 0, 0), (WM_ACTIVATEAPP, 0, 0), (WM_KILLFOCUS, 0, 0)]);
    user32::feed(Input::Focus(false), false);
    assert_eq!(messages(), [], "only changes");

    user32::feed(Input::Minimized(true), false);
    assert_eq!(messages(), [(WM_SIZE, SIZE_MINIMIZED, 0)]);
    user32::feed(Input::Minimized(false), false);
    assert_eq!(messages(), [(WM_SIZE, SIZE_RESTORED, 1024 | 768 << 16)]);

    user32::feed(Input::Focus(true), false);
    assert_eq!(messages(), [(WM_ACTIVATEAPP, 1, 0), (WM_ACTIVATE, 1, 0), (WM_SETFOCUS, 0, 0)]);
}

#[test]
fn simulated_loss_is_lost_then_waits_for_reset() {
    let now = Instant::now();
    assert_eq!(cooperative_level(0, None, now), 0, "no simulation: the host's answer");
    assert_eq!(cooperative_level(D3DERR_DEVICELOST, None, now), D3DERR_DEVICELOST);
    let until = Some(now + Duration::from_secs(2));
    assert_eq!(cooperative_level(0, until, now), D3DERR_DEVICELOST);
    assert_eq!(cooperative_level(0, until, now + Duration::from_secs(3)), D3DERR_DEVICENOTRESET);
}

/// With the D3D9 worker, TestCooperativeLevel answers from the worker's last Present while the
/// device is fine; once that said lost, the host is asked again (a real loss must reach Reset).
#[test]
fn queued_cooperative_level() {
    use rust_metal::platform::d3d9::device::queued_coop;
    let mut asked = false;
    assert_eq!(queued_coop(0, || { asked = true; D3DERR_DEVICELOST }), 0);
    assert!(!asked, "a fine device needs no host call");
    assert_eq!(queued_coop(D3DERR_DEVICELOST, || D3DERR_DEVICENOTRESET), D3DERR_DEVICENOTRESET);
}
