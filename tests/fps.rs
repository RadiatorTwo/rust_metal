//! Places drawn between two logic ticks (`fps::blend`): positions and scales on the line between
//! them, rotations turned part of the way. Needs no game files.

use rust_metal::platform::fps::{PLACE_N, blend};

/// A place turned `deg` about the vertical axis, at `pos`, scale 1.
fn turned(deg: f32, pos: [f32; 3]) -> [f32; PLACE_N] {
    let (s, c) = deg.to_radians().sin_cos();
    let mut p = [c, 0.0, -s, 0.0, 1.0, 0.0, s, 0.0, c, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0];
    p[9..12].copy_from_slice(&pos);
    p
}

fn close(a: &[f32], b: &[f32]) -> bool {
    a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-4)
}

#[test]
fn halfway_between_two_ticks() {
    let (a, b) = (turned(0.0, [0.0, 0.0, 0.0]), turned(90.0, [10.0, 20.0, -40.0]));
    let mid = blend(&a, &b, PLACE_N, 0.5);
    assert!(close(&mid, &turned(45.0, [5.0, 10.0, -20.0])), "{mid:?}");
}

#[test]
fn ends_are_the_ticks() {
    let (a, b) = (turned(10.0, [1.0, 2.0, 3.0]), turned(-30.0, [4.0, 5.0, 6.0]));
    assert!(close(&blend(&a, &b, PLACE_N, 0.0), &a));
    assert!(close(&blend(&a, &b, PLACE_N, 1.0), &b));
}

#[test]
fn the_short_way_round() {
    // from 170 to -170 degrees: through 180, not through 0
    let mid = blend(&turned(170.0, [0.0; 3]), &turned(-170.0, [0.0; 3]), PLACE_N, 0.5);
    assert!(close(&mid[..9], &turned(180.0, [0.0; 3])[..9]), "{mid:?}");
}

#[test]
fn not_a_rotation_is_mixed_as_numbers() {
    let mut a = turned(0.0, [0.0; 3]);
    let mut b = turned(0.0, [0.0; 3]);
    a[0] = 2.0;
    b[0] = 4.0;
    assert_eq!(blend(&a, &b, PLACE_N, 0.5)[0], 3.0);
}
