//! The HUD at any resolution. The game draws it in pixels: positions follow the screen's size,
//! sprite sizes do not, so on a big screen the HUD is small. The HUD's functions see a screen 768
//! pixels high instead (as wide as the real one's shape: 1365x768 for 2560x1440), so they lay
//! it out as at 1024x768, and each overlay page (the HUD's quads, overlay 0x71e690) is scaled up
//! to the real screen when it is drawn (overlay_flush 0x48d8f0).
//!
//! The HUD's screen: render_width/height (0x46eec0, 0x46eee0) and the globals 0x6f99b0/b4 while
//! hud_frame (0x43c210: the HUD's per-frame layout, then hud_draw 0x43d190) and the HUD's set-up
//! (0x43c890, 0x43cf60) run, and the sprite sheets' viewport (sheet_place 0x43cf30). Not for
//! hud_draw's own code: its cinema bars and, at its end, the 3D view set back (0x549910: the
//! viewport and projection) need the real size.

use super::{mem, video};
use crate::rt::{Cpu, EAX, ECX, ESP, call_original};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

pub const HUD_FRAME: u32 = 0x43c210;
pub const HUD_SETUP: u32 = 0x43c890;
pub const HUD_SETUP2: u32 = 0x43cf60;
pub const SHEET_PLACE: u32 = 0x43cf30;
pub const HUD_DRAW: u32 = 0x43d190;
const HUD_DRAW_END: u32 = 0x43d533;
pub const RENDER_WIDTH: u32 = 0x46eec0;
pub const RENDER_HEIGHT: u32 = 0x46eee0;
pub const OVERLAY_FLUSH: u32 = 0x48d8f0;
/// The screen's width and height as the game keeps them (read by world_to_screen 0x549470).
const SCREEN: u32 = 0x6f99b0;
/// The height the HUD is laid out for.
const HEIGHT: f32 = 768.0;

static IN_HUD: AtomicBool = AtomicBool::new(false);

/// The real screen over the HUD's (1 at 768 lines).
fn scale() -> Option<f32> {
    let (w, h) = video::render_size();
    (w > 0 && h > 0).then(|| h as f32 / HEIGHT)
}

/// Runs a function of the HUD with the HUD's screen.
fn with_hud_screen(c: &mut Cpu, f: u32) {
    let m = mem();
    let kept = (m.rd32(SCREEN), m.rd32(SCREEN + 4));
    if let Some(s) = scale() {
        m.wr32(SCREEN, (kept.0 as f32 / s).round() as u32);
        m.wr32(SCREEN + 4, (kept.1 as f32 / s).round() as u32);
    }
    let was = IN_HUD.swap(true, Ordering::Relaxed);
    call_original(c, f);
    IN_HUD.store(was, Ordering::Relaxed);
    m.wr32(SCREEN, kept.0);
    m.wr32(SCREEN + 4, kept.1);
}

/// The HUD's copies of the players' parts of the screen (52 bytes each from 0x656980, x, y,
/// width, height at +0x10; set by 0x43bf20 with the sheets): hud_frame turns them into fractions
/// of the screen, so it sees them in the HUD's screen; hud_draw sets the 3D view back from them,
/// so it sees the real ones.
const PARTS: u32 = 0x656980;
const PLAYERS: u32 = 4;
static REAL_PARTS: Mutex<Option<[[u32; 4]; 4]>> = Mutex::new(None);

fn part_fields() -> impl Iterator<Item = (usize, usize, u32)> {
    (0..PLAYERS).flat_map(|p| (0..4).map(move |k| (p as usize, k as usize, PARTS + 52 * p + 0x10 + 4 * k)))
}

pub fn hud_frame(c: &mut Cpu) {
    let m = mem();
    let s = scale();
    if let Some(s) = s {
        let mut real = [[0; 4]; 4];
        for (p, k, a) in part_fields() {
            real[p][k] = m.rd32(a);
            m.wr32(a, (m.rd32(a) as i32 as f32 / s).round() as i32 as u32);
        }
        *REAL_PARTS.lock().unwrap() = Some(real);
    }
    with_hud_screen(c, HUD_FRAME);
    if let Some(real) = REAL_PARTS.lock().unwrap().take() {
        for (p, k, a) in part_fields() {
            m.wr32(a, real[p][k]);
        }
    }
}

pub fn hud_draw(c: &mut Cpu) {
    let real = *REAL_PARTS.lock().unwrap();
    let Some(real) = real else {
        return call_original(c, HUD_DRAW);
    };
    let m = mem();
    let hud: Vec<u32> = part_fields().map(|(_, _, a)| m.rd32(a)).collect();
    for (p, k, a) in part_fields() {
        m.wr32(a, real[p][k]);
    }
    call_original(c, HUD_DRAW);
    for ((_, _, a), v) in part_fields().zip(hud) {
        m.wr32(a, v);
    }
}

pub fn hud_setup(c: &mut Cpu) {
    with_hud_screen(c, HUD_SETUP)
}

pub fn hud_setup2(c: &mut Cpu) {
    with_hud_screen(c, HUD_SETUP2)
}

/// thiscall on a HUD sprite sheet (sheet_place(viewport)): the player's part of the screen (x,
/// y, width, height at +0x10 of the viewport) as the sheet's origin (+0, +4) and size (+0x10,
/// +0x14), which place sprites by fractions (0x43d880, 0x43d8b0): in the HUD's screen.
pub fn sheet_place(c: &mut Cpu) {
    let sheet = c.r[ECX];
    call_original(c, SHEET_PLACE);
    if let Some(s) = scale() {
        let m = mem();
        for off in [0, 4, 0x10, 0x14] {
            m.wr32(sheet + off, (m.rd32(sheet + off) as i32 as f32 / s).round() as i32 as u32);
        }
    }
}

/// thiscall on the renderer, no arguments: the screen's size, the HUD's in the HUD's functions.
fn size(c: &mut Cpu, original: u32, height: bool) {
    let from = mem().rd32(c.r[ESP]);
    let hud = IN_HUD.load(Ordering::Relaxed) && !(HUD_DRAW..HUD_DRAW_END).contains(&from);
    let Some(s) = scale().filter(|_| hud) else {
        return call_original(c, original);
    };
    let (w, h) = video::render_size();
    c.r[EAX] = ((if height { h } else { w }) as f32 / s).round() as u32;
    // ret
    c.r[ESP] += 4;
}

pub fn render_width(c: &mut Cpu) {
    size(c, RENDER_WIDTH, false)
}

pub fn render_height(c: &mut Cpu) {
    size(c, RENDER_HEIGHT, true)
}

/// thiscall on the overlay: draws the open page's quads (vertices at +0x4964, count +0x495c, in
/// the dynamic vertex buffer of the page (+0x4958): +0x36e0 of its 36-byte record, buffers of
/// 56 bytes at 0x6fa86c with the vertex size at +8). Pre-transformed: x and y are pixels.
pub fn overlay_flush(c: &mut Cpu) {
    if let Some(s) = scale().filter(|&s| s != 1.0 && IN_HUD.load(Ordering::Relaxed)) {
        let m = mem();
        let o = c.r[ECX];
        let page = m.rd32(o + 0x4958);
        let buffer = 0x6fa86c + 56 * m.rd32(o + 36 * page + 0x36e0);
        let (at, n, stride) = (m.rd32(o + 0x4964), m.rd32(o + 0x495c), m.rd32(buffer + 8));
        if at != 0 && stride >= 8 {
            for i in 0..n.min(0x10000) {
                for k in 0..2 {
                    let a = at + i * stride + 4 * k;
                    m.wr32(a, (f32::from_bits(m.rd32(a)) * s).to_bits());
                }
            }
        }
    }
    call_original(c, OVERLAY_FLUSH);
}

pub const HOOKS: [(u32, crate::rt::Hook); 8] = [
    (HUD_FRAME, hud_frame),
    (HUD_DRAW, hud_draw),
    (HUD_SETUP, hud_setup),
    (HUD_SETUP2, hud_setup2),
    (SHEET_PLACE, sheet_place),
    (RENDER_WIDTH, render_width),
    (RENDER_HEIGHT, render_height),
    (OVERLAY_FLUSH, overlay_flush),
];
