//! The Direct3D 9 state cache (`d3d9::state`) that answers Get*State while the calls run on
//! the worker thread. No device needed.
#![cfg(windows)]

use rust_metal::platform::d3d9::state::States;

fn filled() -> States {
    let mut s = States::default();
    s.validate();
    s
}

#[test]
fn set_then_get() {
    let mut s = filled();
    s.set_render(7, 1);
    s.set_stage(3, 1, 4);
    s.set_sampler(15, 13, 2);
    assert_eq!(s.render(7), Some(1));
    assert_eq!(s.stage(3, 1), Some(4));
    assert_eq!(s.sampler(15, 13), Some(2));
}

#[test]
fn out_of_range_is_none() {
    let mut s = filled();
    s.set_render(256, 1);
    s.set_stage(8, 0, 1);
    s.set_sampler(16, 0, 1);
    assert_eq!(s.render(256), None);
    assert_eq!(s.stage(8, 0), None);
    assert_eq!(s.sampler(16, 0), None);
}

#[test]
fn invalid_is_none() {
    let mut s = filled();
    s.set_render(7, 1);
    s.invalidate();
    assert_eq!(s.render(7), None);
    // a fresh cache knows nothing either
    assert_eq!(States::default().render(7), None);
}
