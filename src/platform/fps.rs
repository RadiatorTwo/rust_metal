//! The frame rate. In missions game_frame (0x45e360) runs with Game/variableUpdate on, as the
//! game's own settings say (maxFrameTime 40, variableUpdate 1, variableUpdateTime 40): per
//! frame it runs (real clock - game time) / 40 logic ticks of 40 ms each (0 to 3; the real
//! clock is proj_data+0x360) and draws, with no Sleep. So frames are not capped and the game's
//! speed follows the real clock, but the logic moves in 40 ms steps (25 Hz) and the game
//! interpolates nothing: between two ticks every picture is the same.
//!
//! Smooth pictures: what is drawn is put between its places at the last two ticks, by how far
//! the real clock is past the last tick (so pictures show the game up to one tick late). The
//! logic is not touched: each value is changed only for the call that draws with it and put
//! back right after; the camera's globals are set again with the real values when the frame
//! ends. Interpolated so far:
//! - the camera: cam_set (0x5491e0, cdecl(position, 3x3 rotation)), called by 0x40c2b0 with
//!   the camera's +0x1cc and +0x1a8; it sets the globals 0x72a7c8 (position) and 0x72a7d8
//!   (matrix) and the renderer's view (0x4707e0, SetTransform VIEW);
//! - model nodes: node_draw (0x5506c0, thiscall on a node) computes the node's world place at
//!   +0x40 (3x3 rotation, position +0x24, scale +0x30: 15 floats) and hands it to
//!   set_world (0x490610), which builds the WORLD matrix: ~470 calls a frame in a mission;
//! - particles: the game keeps each emitter's place at the last two ticks and blends them by a
//!   `t` it always passes as 0 (effects_draw 0x5123a0): we pass how far between the ticks;
//! - trails (trail_draw 0x53a2c0): histories of points that move on by one at each tick.
//!
//! Still at the tick: water (0x530da0, "Water", its waves if they move), the radar's dots
//! (0x444035, 0x4440e6). Target markers are projected every frame with the drawn camera, onto
//! a target's place at the last tick (off by at most what it moves in 40 ms).
//!
//! A value is known by its address and how many times it was drawn before in the same frame
//! (one node can be drawn more than once). It is interpolated only when it was seen at the
//! tick before the current one and still holds what it held at the current tick; rotations
//! are slerped as quaternions. Registry value `Smooth` (DWORD, in the game's key): 0 turns it
//! off (pictures as the original draws them).
//!
//! `GUNMETAL_STEPS=1` (a finding aid): where game_step_ms (0x534eb0) is asked outside the
//! logic ticks, by return address, logged every 300 frames with the game time. Those places
//! advance something per drawn frame by the logic's step. Seen in a mission: only 0x45ef16
//! (game_frame's call of 0x44d9f0, envelopes of a list at 0x661d6c whose results are not
//! stored: probably the Xbox version's rumble).

use super::{advapi32, frames, guest_call, log, mem};
use crate::rt::{Cpu, ECX, ESP, call_original};
use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

pub const GAME_FRAME: u32 = 0x45e360;
pub const GAME_STEP_MS: u32 = 0x534eb0;
pub const CAM_SET: u32 = 0x5491e0;
pub const SET_WORLD: u32 = 0x490610;
pub const EFFECTS_DRAW: u32 = 0x5123a0;
pub const TRAIL_DRAW: u32 = 0x53a2c0;
/// A pointer to the project's data: real clock +0x360, variableUpdate +0x4c4,
/// variableUpdateTime +0x4c8, game time +0x4dc.
pub const PROJ_DATA: u32 = 0x72a458;

/// What game_frame's tick loop calls (0x45ed8b..0x45eea3): drawing inside them is not
/// interpolated, and game_step_ms asked inside them is the logic's.
pub const TICK_PARTS: [u32; 7] = [0x534e70, 0x457640, 0x4a5620, 0x4c49d0, 0x449430, 0x44c300, 0x413c60];

/// Floats of a place: a 3x3 rotation, a position and (nodes) a scale.
const ROT: usize = 9;
pub const PLACE_N: usize = 15;
/// A place that moved further than this in one tick jumped (or its memory is something
/// else's now): not interpolated.
const JUMP: f32 = 4000.0;

static IN_TICK: AtomicU32 = AtomicU32::new(0);
static IN_FRAME: AtomicBool = AtomicBool::new(false);
static STEPS: Mutex<BTreeMap<u32, u32>> = Mutex::new(BTreeMap::new());
static FRAMES: AtomicU32 = AtomicU32::new(0);

/// A drawn value at the last two ticks it was seen at.
#[derive(Clone, Copy)]
struct Track {
    prev: [f32; PLACE_N],
    cur: [f32; PLACE_N],
    prev_t: u32,
    cur_t: u32,
    /// the frame it was last drawn in
    frame: u32,
}

#[derive(Default)]
struct State {
    tracks: HashMap<(u32, u32), Track>,
    /// draws of each address in this frame
    seen: HashMap<u32, u32>,
    /// the camera's last cam_set in this frame (position, rotation), if it was changed
    camera: Option<(u32, u32)>,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn finding() -> bool {
    static F: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *F.get_or_init(|| std::env::var_os("GUNMETAL_STEPS").is_some())
}

/// Smooth pictures chosen (the default).
fn smooth() -> bool {
    static S: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *S.get_or_init(|| {
        let v = advapi32::value("", "Smooth").and_then(|(_, d)| Some(u32::from_le_bytes(d.get(..4)?.try_into().ok()?)));
        v != Some(0)
    })
}

pub fn game_frame(c: &mut Cpu) {
    if let Some(s) = STATE.lock().unwrap().as_mut() {
        s.seen.clear();
        s.camera = None;
    }
    let log = frames::on();
    if log {
        frames::frame_start();
    }
    IN_FRAME.store(true, Ordering::Relaxed);
    call_original(c, GAME_FRAME);
    IN_FRAME.store(false, Ordering::Relaxed);
    if log {
        frames::frame_end();
    }
    let camera = STATE.lock().unwrap().as_mut().and_then(|s| s.camera.take());
    if let Some((pos, rot)) = camera {
        // the camera's globals as the logic left them (registers kept: eax is the result)
        let r = c.r;
        guest_call(c, CAM_SET, &[pos, rot]);
        c.r = r;
    }
    let n = FRAMES.fetch_add(1, Ordering::Relaxed) + 1;
    if n % 600 == 0 {
        if let Some(s) = STATE.lock().unwrap().as_mut() {
            s.tracks.retain(|_, t| n.wrapping_sub(t.frame) < 600);
        }
    }
    if finding() && n % 300 == 0 {
        let s = std::mem::take(&mut *STEPS.lock().unwrap());
        let list: Vec<String> = s.iter().map(|(a, k)| format!("{a:#x}:{k}")).collect();
        let p = mem().rd32(PROJ_DATA);
        log!("fps: {n} frames at {} ms, game time {}; game_step_ms outside ticks: {}", super::kernel32::ms_since_start(), mem().rd32(p + 0x4dc), list.join(" "));
    }
}

/// How far between the last two ticks to draw (0..1), or None: not now.
fn alpha() -> Option<f32> {
    if !smooth() || !IN_FRAME.load(Ordering::Relaxed) || IN_TICK.load(Ordering::Relaxed) != 0 {
        return None;
    }
    let m = mem();
    let p = m.rd32(PROJ_DATA);
    let step = m.rd32(p + 0x4c8) as i32;
    if m.rd32(p + 0x4c4) == 0 || step <= 0 {
        return None;
    }
    let ahead = m.rd32(p + 0x360).wrapping_sub(m.rd32(p + 0x4dc)) as i32;
    Some((ahead as f32 / step as f32).clamp(0.0, 1.0))
}

/// The place to draw for the value `now` at `key` (its address), or None: draw it as it is.
fn place(key: u32, now: &[f32; PLACE_N], n: usize, alpha: f32) -> Option<[f32; PLACE_N]> {
    let m = mem();
    let p = m.rd32(PROJ_DATA);
    let (t, step) = (m.rd32(p + 0x4dc), m.rd32(p + 0x4c8));
    let frame = FRAMES.load(Ordering::Relaxed);
    let mut guard = STATE.lock().unwrap();
    let s = guard.get_or_insert_with(State::default);
    let k = s.seen.entry(key).or_insert(0);
    let occurrence = *k;
    *k += 1;
    let tr = s.tracks.entry((key, occurrence)).or_insert(Track { prev: *now, cur: *now, prev_t: t, cur_t: t, frame });
    tr.frame = frame;
    if tr.cur_t != t {
        tr.prev = tr.cur;
        tr.prev_t = tr.cur_t;
        tr.cur_t = t;
        tr.cur = *now;
    } else if tr.cur[..n] != now[..n] {
        // changed since the tick (not ours to smooth)
        tr.cur = *now;
        tr.prev = *now;
        return None;
    }
    if tr.prev_t.wrapping_add(step) != tr.cur_t {
        return None;
    }
    let d: f32 = (ROT..ROT + 3).map(|i| (tr.cur[i] - tr.prev[i]).powi(2)).sum::<f32>().sqrt();
    if !(d < JUMP) {
        return None;
    }
    Some(blend(&tr.prev, &tr.cur, n, alpha))
}

/// prev + (cur - prev) * a; the rotation slerped when both are rotations.
pub fn blend(prev: &[f32; PLACE_N], cur: &[f32; PLACE_N], n: usize, a: f32) -> [f32; PLACE_N] {
    let mut out = *cur;
    for i in 0..n {
        out[i] = prev[i] + (cur[i] - prev[i]) * a;
    }
    if let (Some(q0), Some(q1)) = (quat(prev), quat(cur)) {
        out[..ROT].copy_from_slice(&matrix(slerp(q0, q1, a)));
    }
    out
}

/// The rotation's quaternion, if the 3x3 is one (orthonormal, not mirrored).
fn quat(m: &[f32; PLACE_N]) -> Option<[f32; 4]> {
    let r = |i: usize, j: usize| m[3 * i + j];
    for i in 0..3 {
        let len = (0..3).map(|j| r(i, j) * r(i, j)).sum::<f32>();
        if (len - 1.0).abs() > 0.02 {
            return None;
        }
    }
    let det = r(0, 0) * (r(1, 1) * r(2, 2) - r(1, 2) * r(2, 1)) - r(0, 1) * (r(1, 0) * r(2, 2) - r(1, 2) * r(2, 0))
        + r(0, 2) * (r(1, 0) * r(2, 1) - r(1, 1) * r(2, 0));
    if (det - 1.0).abs() > 0.05 {
        return None;
    }
    let tr = r(0, 0) + r(1, 1) + r(2, 2);
    let q = if tr > 0.0 {
        let s = (tr + 1.0).sqrt() * 2.0;
        [0.25 * s, (r(2, 1) - r(1, 2)) / s, (r(0, 2) - r(2, 0)) / s, (r(1, 0) - r(0, 1)) / s]
    } else if r(0, 0) > r(1, 1) && r(0, 0) > r(2, 2) {
        let s = (1.0 + r(0, 0) - r(1, 1) - r(2, 2)).sqrt() * 2.0;
        [(r(2, 1) - r(1, 2)) / s, 0.25 * s, (r(0, 1) + r(1, 0)) / s, (r(0, 2) + r(2, 0)) / s]
    } else if r(1, 1) > r(2, 2) {
        let s = (1.0 + r(1, 1) - r(0, 0) - r(2, 2)).sqrt() * 2.0;
        [(r(0, 2) - r(2, 0)) / s, (r(0, 1) + r(1, 0)) / s, 0.25 * s, (r(1, 2) + r(2, 1)) / s]
    } else {
        let s = (1.0 + r(2, 2) - r(0, 0) - r(1, 1)).sqrt() * 2.0;
        [(r(1, 0) - r(0, 1)) / s, (r(0, 2) + r(2, 0)) / s, (r(1, 2) + r(2, 1)) / s, 0.25 * s]
    };
    Some(q)
}

/// The 3x3 rotation of a unit quaternion (w, x, y, z), the inverse of `quat`.
fn matrix([w, x, y, z]: [f32; 4]) -> [f32; ROT] {
    [
        1.0 - 2.0 * (y * y + z * z),
        2.0 * (x * y - w * z),
        2.0 * (x * z + w * y),
        2.0 * (x * y + w * z),
        1.0 - 2.0 * (x * x + z * z),
        2.0 * (y * z - w * x),
        2.0 * (x * z - w * y),
        2.0 * (y * z + w * x),
        1.0 - 2.0 * (x * x + y * y),
    ]
}

fn slerp(a: [f32; 4], mut b: [f32; 4], t: f32) -> [f32; 4] {
    let mut dot: f32 = (0..4).map(|i| a[i] * b[i]).sum();
    if dot < 0.0 {
        b = b.map(|v| -v);
        dot = -dot;
    }
    let (wa, wb) = if dot > 0.9995 {
        (1.0 - t, t)
    } else {
        let th = dot.min(1.0).acos();
        let s = th.sin();
        (((1.0 - t) * th).sin() / s, (t * th).sin() / s)
    };
    let q: [f32; 4] = std::array::from_fn(|i| wa * a[i] + wb * b[i]);
    let len = q.iter().map(|v| v * v).sum::<f32>().sqrt();
    q.map(|v| v / len)
}

fn read(a: u32, out: &mut [f32]) {
    for (i, v) in out.iter_mut().enumerate() {
        *v = f32::from_bits(mem().rd32(a + 4 * i as u32));
    }
}

fn write(a: u32, v: &[f32]) {
    for (i, v) in v.iter().enumerate() {
        mem().wr32(a + 4 * i as u32, v.to_bits());
    }
}

/// cam_set(position, rotation).
pub fn cam_set(c: &mut Cpu) {
    let m = mem();
    let (pos, rot) = (m.rd32(c.r[ESP] + 4), m.rd32(c.r[ESP] + 8));
    let Some(a) = alpha() else {
        return call_original(c, CAM_SET);
    };
    let mut now = [0.0; PLACE_N];
    read(rot, &mut now[..ROT]);
    read(pos, &mut now[ROT..ROT + 3]);
    let Some(shown) = place(pos, &now, ROT + 3, a) else {
        return call_original(c, CAM_SET);
    };
    write(rot, &shown[..ROT]);
    write(pos, &shown[ROT..ROT + 3]);
    call_original(c, CAM_SET);
    write(rot, &now[..ROT]);
    write(pos, &now[ROT..ROT + 3]);
    if let Some(s) = STATE.lock().unwrap().as_mut() {
        s.camera = Some((pos, rot));
    }
}

/// set_world(place): a node's world place.
pub fn set_world(c: &mut Cpu) {
    let at = mem().rd32(c.r[ESP] + 4);
    let Some(a) = alpha() else {
        return call_original(c, SET_WORLD);
    };
    let mut now = [0.0; PLACE_N];
    read(at, &mut now);
    let Some(shown) = place(at, &now, PLACE_N, a) else {
        return call_original(c, SET_WORLD);
    };
    write(at, &shown);
    call_original(c, SET_WORLD);
    write(at, &now);
}

/// effects_draw(?, t): draws the particle systems, each emitter between its places at the last
/// two ticks (+0x10 and +0xc of an emitter, 0x543b90) by `t`; the game always passes 0 (the
/// older place), we pass how far the clock is past the last tick. The value is kept only for
/// the call (this+0x20 and the effect manager's +0x10, which only the drawing reads).
pub fn effects_draw(c: &mut Cpu) {
    let Some(a) = alpha() else {
        return call_original(c, EFFECTS_DRAW);
    };
    let m = mem();
    let this = c.r[ECX];
    let manager = m.rd32(this + 4);
    let kept = (m.rd32(this + 0x20), m.rd32(manager + 0x10));
    m.wr32(c.r[ESP] + 8, a.to_bits());
    call_original(c, EFFECTS_DRAW);
    m.wr32(this + 0x20, kept.0);
    m.wr32(manager + 0x10, kept.1);
}

/// trail_draw(pass, t) (0x53a2c0, a trail's drawing; t is not used): the trail is a history of
/// points that moves on by one at each tick (0x53a190): point j of a strand is the tick's point
/// j - 1, point 0 its source. Each point is drawn between its last two ticks; only in the pass it
/// draws in, so that occurrences count the same draws.
pub fn trail_draw(c: &mut Cpu) {
    let m = mem();
    let this = c.r[ECX];
    let def = m.rd32(this + 0x94);
    let drawn = m.rd32(c.r[ESP] + 4) == m.rd32(def + 0x28) && m.rd32(this + 0xa8) != u32::MAX;
    let Some(a) = alpha().filter(|_| drawn) else {
        return call_original(c, TRAIL_DRAW);
    };
    let (strands, len) = (m.rd32(this + 8), m.rd32(def + 0x2c) + 1);
    let rows = m.rd32(this + 0xac);
    let mut kept = Vec::new();
    for j in 0..len.min(1024) {
        let row = m.rd32(rows + 4 * j);
        for i in 0..strands.min(1024) {
            let at = row + 12 * i;
            let mut now = [0.0; PLACE_N];
            read(at, &mut now[ROT..ROT + 3]);
            if let Some(shown) = place(at, &now, ROT + 3, a) {
                write(at, &shown[ROT..ROT + 3]);
                kept.push((at, now));
            }
        }
    }
    call_original(c, TRAIL_DRAW);
    for (at, now) in kept {
        write(at, &now[ROT..ROT + 3]);
    }
}

pub fn game_step_ms(c: &mut Cpu) {
    if finding() && IN_TICK.load(Ordering::Relaxed) == 0 {
        let ret = mem().rd32(c.r[ESP]);
        *STEPS.lock().unwrap().entry(ret).or_default() += 1;
    }
    call_original(c, GAME_STEP_MS);
}

/// The main thread is inside a tick part.
pub fn in_tick() -> bool {
    IN_TICK.load(Ordering::Relaxed) != 0
}

fn tick_part(c: &mut Cpu, i: usize) {
    let outer = IN_TICK.fetch_add(1, Ordering::Relaxed) == 0;
    let t = (outer && frames::on()).then(frames::part_start);
    call_original(c, TICK_PARTS[i]);
    IN_TICK.fetch_sub(1, Ordering::Relaxed);
    if let Some(t) = t {
        frames::part_end(i, t);
    }
}

macro_rules! tick_parts {
    ($($i:literal),*) => { [$((TICK_PARTS[$i], (|c: &mut Cpu| tick_part(c, $i)) as crate::rt::Hook)),*] };
}

pub const HOOKS: [(u32, crate::rt::Hook); 13] = {
    let t = tick_parts!(0, 1, 2, 3, 4, 5, 6);
    [(GAME_FRAME, game_frame), (GAME_STEP_MS, game_step_ms), (CAM_SET, cam_set), (SET_WORLD, set_world), t[0], t[1], t[2], t[3], t[4], t[5], t[6], (EFFECTS_DRAW, effects_draw), (TRAIL_DRAW, trail_draw)]
};
