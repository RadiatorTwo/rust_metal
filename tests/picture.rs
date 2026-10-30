//! Where the game's picture goes in our window: as large as fits with its aspect ratio,
//! centred. Needs no game files and no window.

use rust_metal::platform::window::picture;

#[test]
fn picture_keeps_its_aspect_ratio() {
    // 4:3 on a 16:9 display: bars left and right
    assert_eq!(picture((2560, 1440), (1024, 768)), [320, 0, 1920, 1440]);
    // the window the size of the picture: all of it
    assert_eq!(picture((1024, 768), (1024, 768)), [0, 0, 1024, 768]);
    // a tall window: bars above and below
    assert_eq!(picture((800, 1000), (1024, 768)), [0, 200, 800, 600]);
    // an empty client area does not divide by zero
    assert_eq!(picture((0, 0), (1024, 768)), [0, 0, 1, 1]);
}
