//! C runtime functions of the game done by the host: they are called so often that the
//! recompiled code costs much of a frame.
//!
//! `__ftol` (0x5870ac): st(0) to a 64-bit integer, truncated, popped, in edx:eax. The original
//! switches the FPU's rounding mode twice per call (fnstcw, fldcw), which is slow in
//! recompiled x87.
//!
//! Also the game's warning report (`game_warning`), which goes to our log.

use crate::rt::{Cpu, EAX, EDX, ESP};

/// The game's warning report (0x557f40, cdecl: message, expression, source file, line): it
/// formats an "Assertion Failed" text for game_log (0x557ce0), which writes only the first
/// five messages of a run to gunmetal.log. Ours writes every one to our log and returns.
pub const GAME_WARNING: u32 = 0x557f40;

pub fn game_warning(c: &mut Cpu) {
    let a = |n: u32| super::mem().rd32(c.r[ESP] + 4 + 4 * n);
    let s = |p: u32| if p == 0 { String::new() } else { super::cstr(p) };
    super::log!("game: {} ({}, {}:{})", s(a(0)).trim_end(), s(a(1)), s(a(2)).rsplit('\\').next().unwrap_or(""), a(3));
    c.r[ESP] += 4;
}

pub const FTOL: u32 = 0x5870ac;

pub fn ftol(c: &mut Cpu) {
    let v = c.st(0);
    c.fpop();
    // out of range and NaN: the "integer indefinite" value fistp stores
    let i = if v.is_nan() || v >= 9.223_372_036_854_775_807e18 || v < -9.223_372_036_854_775_808e18 { i64::MIN } else { v.trunc() as i64 };
    c.r[EAX] = i as u32;
    c.r[EDX] = (i >> 32) as u32;
    // ret
    c.r[ESP] += 4;
}
