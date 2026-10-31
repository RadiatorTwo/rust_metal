//! The game's video options.
//!
//! Resolutions: the list (Optionen > Video) holds only 640x480 to 1600x1200: game_start
//! (0x459743) enumerates the display modes through a filter with those limits (unless the
//! options' +0x9c is set), and the list's count (0x46dbc0) is cut at 1600x1200 again when the
//! options' +0x36d8 is at most 1. We lift both, so the display's own resolutions (1920x1080,
//! 2560x1440, ...) can be chosen.
//!
//! Aspect ratio: the game knows the monitor's shape (Graphics/MonitorType, proj_data+0x354: 4:3
//! or 16:9) and the picture's (Graphics/AspectRatio, +0x358, the menu's "Seitenverhältnis":
//! 4:3 = "Normal", 1.66, 16:9, 1.85 ("Breitwand"), 2.00, 2.35, AUTO). A picture narrower than the
//! monitor gets bars at the sides, a wider one at the top and bottom (viewport_set 0x5496f0).
//! The monitor is our back buffer now, whatever its shape (square pixels: our window keeps the
//! aspect ratio): aspect_auto (0x549600, the monitor's shape for the viewport and AUTO) gives the
//! screen's width / height, and the picture's 4:3 (entry 0 of aspect_value 0x5499a0, "Normal")
//! is the back buffer's shape too. So in a mission the 3D view fills any display undistorted and
//! sees more at the sides (Hor+), and the menus, which ask for 4:3 themselves, stand in the
//! middle with black sides (the game draws only its 4:3 part: `d3d9` clears the rest).

use super::mem;
use crate::rt::{Cpu, ESP, call_original};

/// thiscall on 0x6757e8 (filter: min width, min height, max width, max height, ..., flags
/// +0x18): the adapters' display modes the filter lets through (d3d_enum_adapters 0x469760).
pub const ENUM_MODES: u32 = 0x469740;
/// thiscall(adapter, format, only up to 1600x1200): how many display modes to offer.
pub const MODE_COUNT: u32 = 0x46dbc0;
/// cdecl(aspect ratio 0..6) -> st0: width / height of 4:3, 1.66:1, 16:9, 1.85:1, 2:1, 2.35:1.
pub const ASPECT_VALUE: u32 = 0x5499a0;
/// The renderer: its back buffer's size at +0x4ba4c/+0x4ba50, or at [+0x24] when +0x1a is set
/// (render_width 0x46eec0, render_height 0x46eee0).
pub(super) const RENDERER: u32 = 0x675d40;
/// cdecl(monitor type, screen width, screen height) -> st0: the monitor's shape, as seen through
/// a screen of that size.
pub const ASPECT_AUTO: u32 = 0x549600;

pub fn enum_modes(c: &mut Cpu) {
    let filter = mem().rd32(c.r[ESP] + 4);
    mem().wr32(filter + 8, i32::MAX as u32);
    mem().wr32(filter + 12, i32::MAX as u32);
    call_original(c, ENUM_MODES);
}

pub fn mode_count(c: &mut Cpu) {
    mem().wr32(c.r[ESP] + 12, 0);
    call_original(c, MODE_COUNT);
}

/// The back buffer's width and height.
pub(super) fn render_size() -> (u32, u32) {
    let m = mem();
    if m.rd8(RENDERER + 0x1a) != 0 {
        let s = m.rd32(RENDERER + 0x24);
        (m.rd32(s), m.rd32(s + 4))
    } else {
        (m.rd32(RENDERER + 0x4ba4c), m.rd32(RENDERER + 0x4ba50))
    }
}

/// A screen narrower than 4:3 (5:4, 1280x1024) keeps the game's own aspect handling: its
/// viewport math (0x549a50) assumes the picture is at least as wide as 4:3 and would make a
/// viewport taller than the back buffer, which ValidateDevice refuses (the front end's
/// "vc_base.mtl failed to validate"). There the picture is 4:3 stretched to the screen, as in
/// the original.
fn narrow(w: u32, h: u32) -> bool {
    w == 0 || h == 0 || (w as u64) * 3 < (h as u64) * 4
}

pub fn aspect_value(c: &mut Cpu) {
    let (w, h) = render_size();
    if mem().rd32(c.r[ESP] + 4) != 0 || narrow(w, h) {
        return call_original(c, ASPECT_VALUE);
    }
    c.fpush(w as f64 / h as f64);
    // ret
    c.r[ESP] += 4;
}

/// The monitor is the back buffer: its shape through a screen of w x h is w / h.
pub fn aspect_auto(c: &mut Cpu) {
    let m = mem();
    let (w, h) = (m.rd32(c.r[ESP] + 8), m.rd32(c.r[ESP] + 12));
    if narrow(w, h) {
        return call_original(c, ASPECT_AUTO);
    }
    c.fpush(w as f64 / h as f64);
    // ret
    c.r[ESP] += 4;
}

pub const HOOKS: [(u32, crate::rt::Hook); 4] =
    [(ENUM_MODES, enum_modes), (MODE_COUNT, mode_count), (ASPECT_VALUE, aspect_value), (ASPECT_AUTO, aspect_auto)];
