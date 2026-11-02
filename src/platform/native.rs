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

pub const MAT_EVAL: u32 = 0x47db50;

/// The game's state object (0x675d40): vertex shader constants (96 x 4 floats) at +0x87610
/// with the lowest and highest register changed at +0x87c10/+0x87c14 (uploaded by 0x47cb30),
/// pixel shader constants (8 x 4) at +0x87c18 with their range at +0x87c98/+0x87c9c.
const STATE: u32 = 0x675d40;
const TIME: u32 = 0x6c1ba8;

/// The number of stack values an opcode takes and leaves, for the ones done natively.
fn mat_op(op: u32) -> Option<(u32, u32)> {
    Some(match op {
        0..=3 => (2, 1),
        4 | 11 | 12 | 37 | 39 => (1, 1),
        6 | 10 | 13 | 31 | 34 | 48 => (0, 1),
        7 | 8 | 41 => (3, 1),
        27 | 28 => (3, 0),
        46 => (4, 1),
        _ => return None,
    })
}

/// thiscall (index 0..13, material) -> st0: material parameter `index` of a material's
/// parameter set (`this`), a little stack program of 8-byte entries (type 1: push the float
/// value; type 0: opcode) at [this + 4 index], its length at [this + 0x38 + 4 index], the
/// stack 32 doubles at this + 0x70, its depth at this + 0x170. An empty program gives the
/// index's default (1, 0 or infinity). Index 13 is run for its effects (opcodes 27 and 28 set
/// shader constants) and gives 0. Done natively when every opcode is one of `mat_op`'s and
/// the stack's depth works out (checked before running, as the effects cannot be undone);
/// else, or with a warning the original would give, the original runs. Each result is
/// rounded to a float's precision: the game runs the FPU at 24 bits.
pub fn mat_eval(c: &mut Cpu) {
    if !on() {
        return crate::rt::call_original(c, MAT_EVAL);
    }
    let m = super::mem();
    let this = c.r[crate::rt::ECX];
    let idx = m.rd32(c.r[ESP] + 4);
    if idx > 13 {
        return crate::rt::call_original(c, MAT_EVAL);
    }
    let n = m.rd32(this + 0x38 + idx * 4) as i32;
    let prog = m.rd32(this + idx * 4);
    let result = if n <= 0 {
        m.wr32(this + 0x170, 0);
        match [0, 0, 1, 1, 1, 1, 1, 1, 1, 2, 2, 2, 2, 1][idx as usize] {
            0 => 1.0,
            1 => 0.0,
            _ => f64::INFINITY,
        }
    } else {
        // the program must be one we can run to its end
        let mut depth = 0u32;
        for i in 0..n as u32 {
            let (t, v) = (m.rd32(prog + i * 8), m.rd32(prog + i * 8 + 4));
            let (take, give) = match t {
                1 => (0, 1),
                0 => match mat_op(v) {
                    Some(e) => e,
                    None => return crate::rt::call_original(c, MAT_EVAL),
                },
                _ => return crate::rt::call_original(c, MAT_EVAL),
            };
            if depth < take || depth - take + give > 32 {
                return crate::rt::call_original(c, MAT_EVAL);
            }
            depth = depth - take + give;
        }
        if depth != (idx != 13) as u32 {
            return crate::rt::call_original(c, MAT_EVAL);
        }
        let r = |x: f64| x as f32 as f64;
        let f = |a: u32| f32::from_bits(m.rd32(a)) as f64;
        let d = |a: u32| f64::from_bits(m.rd32(a) as u64 | (m.rd32(a + 4) as u64) << 32);
        let trunc = |x: f64| if x.abs() < 2147483648.0 { x as i32 } else { i32::MIN };
        let mut st = [0f64; 32];
        let mut k = 0usize;
        for i in 0..n as u32 {
            let (t, v) = (m.rd32(prog + i * 8), m.rd32(prog + i * 8 + 4));
            if t == 1 {
                st[k] = f32::from_bits(v) as f64;
                k += 1;
                continue;
            }
            let (pops, _) = mat_op(v).unwrap();
            let top = k - 1;
            match v {
                0 => st[k - 2] = r(st[top] + st[k - 2]),
                1 => st[k - 2] = r(st[k - 2] - st[top]),
                2 => st[k - 2] = r(st[top] * st[k - 2]),
                3 => st[k - 2] = r(st[k - 2] / st[top]),
                4 => st[top] = -st[top],
                6 => st[k] = f(0x6fad00u32.wrapping_add(m.rd32(0x6fbd00).wrapping_mul(4))),
                7 => {
                    // wrap a into b..c
                    let (a, b, c) = (st[k - 3], st[k - 2], st[top]);
                    let cb = r(c - b);
                    let mut w = r(r(a - b) / cb) % 1.0;
                    if w < 0.0 {
                        w = r(w + 1.0);
                    }
                    st[k - 3] = r(r(cb * w) + b);
                }
                8 => {
                    // clamp a to b..c
                    let (a, b, c) = (st[k - 3], st[k - 2], st[top]);
                    let x = if a < b { b } else { a };
                    st[k - 3] = if x > c { c } else { x };
                }
                10 => st[k] = d(TIME),
                11 => st[top] = r(r(st[top] * d(0x5e5b70)).sin()),
                12 => st[top] = r(r(st[top] * d(0x5e5b70)).cos()),
                13 => st[k] = f64::from_bits(0x400921fb_60000000),
                27 | 28 => {
                    // shader constant (register, component) = value
                    let (val, reg, comp) = (st[k - 3] as f32, trunc(st[k - 2]), trunc(st[top]));
                    let (base, lo, regs) = if v == 27 { (STATE + 0x87610, STATE + 0x87c10, 96) } else { (STATE + 0x87c18, STATE + 0x87c98, 8) };
                    if !(0..regs).contains(&reg) || !(0..4).contains(&comp) {
                        return crate::rt::call_original(c, MAT_EVAL);
                    }
                    m.wr32(base + (reg * 4 + comp) as u32 * 4, val.to_bits());
                    if reg < m.rd32(lo) as i32 {
                        m.wr32(lo, reg as u32);
                    }
                    if reg > m.rd32(lo + 4) as i32 {
                        m.wr32(lo + 4, reg as u32);
                    }
                }
                31 => {
                    let (x, y, z) = (f(0x6bc2b4), f(0x6bc2b8), f(0x6bc2bc));
                    st[k] = r(r(r(r(z * z) + r(y * y)) + r(x * x)).sqrt());
                }
                34 => st[k] = r(d(TIME) - f(m.rd32(c.r[ESP] + 8) + 0x138)),
                37 => {
                    let x = st[top];
                    let w = r(x - x.floor());
                    st[top] = r(r(r(r(r(w * d(0x5e4310)) + d(0x5e5b30)).sin()) + 1.0) * 0.5);
                }
                39 => st[top] = r(st[top] - st[top].floor()),
                41 => {
                    // b + (c - b) a
                    let (a, b, c) = (st[k - 3], st[k - 2], st[top]);
                    st[k - 3] = r(r(r(c - b) * a) + b);
                }
                46 => st[k - 4] = if st[k - 4] < st[k - 3] { st[top] } else { st[k - 2] },
                48 => st[k] = m.rd32(0x6c1778) as i32 as f64,
                _ => unreachable!(),
            }
            let (_, give) = mat_op(v).unwrap();
            k = k - pops as usize + give as usize;
        }
        m.wr32(this + 0x170, 0);
        if idx == 13 { 0.0 } else { st[0] }
    };
    c.fpush(result);
    c.r[ESP] += 12;
}

pub const HOOKS: [(u32, crate::rt::Hook); 2] = [(LIGHT_AT, light_at), (MAT_EVAL, mat_eval)];
