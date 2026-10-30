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
//! 4:3 = "Normal", 1.66, 16:9, 1.85 ("Breitwand"), 2.00, 2.35, AUTO); aspect_value (0x5499a0)
//! turns either into a number. A camera's projection takes the picture's: there entry 0 (4:3)
//! is the back buffer's own width / height now (square pixels: our window keeps the aspect
//! ratio), so "Normal" fills any display undistorted and wider ones see more at the sides
//! (Hor+). Everywhere else entry 0 stays 4:3: the viewport and the cinema bars (0x5496f0 with
//! 0x664678, hud_draw) take it as "the monitor's 4:3" and would put bars on a wider picture. The
//! menus are stretched to the back buffer, as the game always did.

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
/// Where a camera's projection takes its aspect (camera_apply 0x40c2b0 and two more: if
/// AspectRatio is AUTO aspect_auto, else aspect_value): only there entry 0 is the picture's own
/// shape.
const CAMERA: [u32; 3] = [0x40c32f, 0x4a4caf, 0x4a5006];

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

pub fn aspect_value(c: &mut Cpu) {
    let (w, h) = render_size();
    let from = mem().rd32(c.r[ESP]);
    if mem().rd32(c.r[ESP] + 4) != 0 || w == 0 || h == 0 || !CAMERA.contains(&from) {
        return call_original(c, ASPECT_VALUE);
    }
    c.fpush(w as f64 / h as f64);
    // ret
    c.r[ESP] += 4;
}

pub const HOOKS: [(u32, crate::rt::Hook); 3] = [(ENUM_MODES, enum_modes), (MODE_COUNT, mode_count), (ASPECT_VALUE, aspect_value)];
