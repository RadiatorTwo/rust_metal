//! Game functions done natively for speed: hot parts of the renderer that the recompiled code
//! runs several times slower than a real CPU (measured with `GUNMETAL_COUNT`, which counts
//! each function's executed instructions). `GUNMETAL_NATIVE=0` runs the originals instead (for
//! comparing pictures and timing).
//!
//! `light_at` (0x449c60): the light colour at a point, called for every object drawn: it walks
//! the whole light list each time (~1200 instructions a call in a battle).

use crate::rt::{Cpu, EAX, ESP};

fn on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("GUNMETAL_NATIVE").map_or(true, |v| v != "0"))
}

pub const LIGHT_AT: u32 = 0x449c60;

/// The light list: an array of 0xb0-byte lights, their number, the first one's index (0xffff:
/// none); each light's next index at +0xae.
const LIGHTS: u32 = 0x661d30;
const LIGHT_COUNT: u32 = 0x661d34;
const LIGHT_FIRST: u32 = 0x661d36;
/// Float constants of the original: 1.0 for the fall-off, the scales of type 0 lights and of
/// the sum.
const ONE: u32 = 0x5e42e0;
const TYPE0_SCALE: u32 = 0x5e5270;
const SUM_SCALE: u32 = 0x5e5244;

/// cdecl (const float *point) -> 0x00RRGGBB: 0.5 plus every enabled light's contribution
/// (type 0: (colour + extra colour) * intensity * scale; type 1, a point light: colour *
/// intensity * (1 - d²/r²) inside its radius r; type 2: nothing), times the sum scale, each
/// rounded to an integer and clamped to 255 (a negative one too, as the original's unsigned
/// compare does). In 32-bit floats: the game runs the FPU at 24-bit precision.
pub fn light_at(c: &mut Cpu) {
    if !on() {
        return crate::rt::call_original(c, LIGHT_AT);
    }
    let m = super::mem();
    let f = |a: u32| f32::from_bits(m.rd32(a));
    let p = m.rd32(c.r[ESP] + 4);
    let (px, py, pz) = (f(p), f(p + 4), f(p + 8));
    let mut sum = [0.5f32; 3];
    let base = m.rd32(LIGHTS);
    let count = m.rd16(LIGHT_COUNT);
    let mut i = m.rd16(LIGHT_FIRST);
    let mut guard = 0u32;
    while i != 0xffff && base != 0 && i < count && guard < 0x10000 {
        guard += 1;
        let l = base + i as u32 * 0xb0;
        if m.rd8(l + 0x40) == 0 && m.rd8(l) & 4 != 0 {
            match m.rd8(l + 2) {
                0 => {
                    let k = f(l + 0xa8) * f(TYPE0_SCALE);
                    for (j, s) in sum.iter_mut().enumerate() {
                        let j = j as u32 * 4;
                        *s += (f(l + 0x28 + j) + f(l + 0x1c + j)) * k;
                    }
                }
                1 => {
                    let (dx, dy, dz) = (f(l + 4) - px, f(l + 8) - py, f(l + 0xc) - pz);
                    let r = f(l + 0x34);
                    let r2 = r * r;
                    let d2 = dz * dz + dy * dy + dx * dx;
                    if d2 < r2 {
                        let k = (f(ONE) - d2 / r2) * f(l + 0xa8);
                        for (j, s) in sum.iter_mut().enumerate() {
                            *s += k * f(l + 0x28 + j as u32 * 4);
                        }
                    }
                }
                _ => {}
            }
        }
        i = m.rd16(l + 0xae);
    }
    let scale = f(SUM_SCALE);
    let byte = |v: f32| {
        let n = (v * scale).round_ties_even();
        // fistp: out of range gives 0x80000000, which the unsigned compare clamps too
        if (0.0..=255.0).contains(&n) { n as u32 } else { 0xff }
    };
    c.r[EAX] = byte(sum[0]) << 16 | byte(sum[1]) << 8 | byte(sum[2]);
    c.r[ESP] += 4;
}

pub const HOOKS: [(u32, crate::rt::Hook); 1] = [(LIGHT_AT, light_at)];
