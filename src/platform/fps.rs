//! The frame rate. In missions game_frame (0x45e360) runs with Game/variableUpdate on, as the
//! game's own settings say (maxFrameTime 40, variableUpdate 1, variableUpdateTime 40): per
//! frame it runs (real clock - game time) / 40 logic ticks of 40 ms each (0 to 3; the real
//! clock is proj_data+0x360) and draws, with no Sleep. So frames are not capped and the game's
//! speed follows the real clock, but the logic moves in 40 ms steps (25 Hz) and the game
//! interpolates nothing: between two ticks every picture is the same.
//!
//! Smooth pictures: what is drawn is put between its places at the last two ticks, by how far
//! the real clock is past the last tick (so pictures show the game up to one tick late). How far
//! is measured when the frame draws, not when the game read its clock at the frame's start: a
//! frame that runs a tick draws ~4 ms later than one that does not, and with the clock of the
//! frame's start its pictures were that much behind (seen 2026-10-31: the camera's movement per
//! picture went up and down 25 times a second, a visible judder in turns and while
//! transforming). The game's clock (proj_data+0x360) is the host's minus a constant (seen:
//! constant over a whole mission to the ms), so the drawing time is the host's high resolution
//! clock minus that offset (`Clock`), fixed for the frame at its first drawing call. The
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
//! - trails (trail_draw 0x53a2c0): histories of points that move on by one at each tick;
//! - the zoom: camera_apply then sets the projection (0x549cb0, cdecl(near, far, aspect * fov,
//!   fov), fov the camera's +0xc0), which changes at each tick while the camera zooms
//!   (transforming with Q): the fov is put between its ticks;
//! - target markers: 0x446dc0 projects two points of a target (at the tick) with
//!   world_to_screen (0x5492a0, cdecl(point, out)) every frame; with the camera between ticks
//!   and the points at the tick they jumped at each tick. Both points are put between their
//!   ticks too (by the return address of the call);
//! - the crosshair: its target (the HUD's +0xd10, on the screen) is the player's aim point at
//!   the tick projected every frame (0x43f160): the point is put between its ticks as the
//!   markers' are. crosshair_draw (0x43ffd0, thiscall(target, ..), called by 0x4409d0) moves
//!   the drawn place (+0xd34) half way to the target each time it draws, that is per frame: at
//!   140 fps it arrived in two frames and stood still until the next tick, steps 25 times a
//!   second (seen 2026-10-31). The step is made one of time: half way per `CROSSHAIR_HALF` ms.
//!
//! Still at the tick: water (0x530da0, "Water", its waves if they move), the radar's dots
//! (0x444035, 0x4440e6). Target markers are projected every frame with the drawn camera, onto
//! a target's place at the last tick (off by at most what it moves in 40 ms).
//!
//! A value is known by its address and its place: one address is drawn many times a frame with
//! different places (grass, units drawn through one node), and which of them are drawn changes
//! with what is in view, so the n-th draw of an address is not the same thing from tick to tick
//! (blending two of them made objects flash to another's place and back). Each address keeps
//! the places drawn at the last two ticks; a place drawn now is blended with the place of the
//! tick before that is nearest to it, when the two are each other's nearest (mutual nearest
//! neighbours), the tick before is the one just before, and the step is not a jump. Places that
//! first appear after the first frame of a tick (changed by the drawing, or newly in view) are
//! drawn as they are. Rotations are slerped as quaternions. Registry value `Smooth` (DWORD, in the game's key): 0 turns it
//! off (pictures as the original draws them).
//!
//! `GUNMETAL_STEPS=1` (a finding aid): where game_step_ms (0x534eb0) is asked outside the
//! logic ticks, by return address, logged every 300 frames with the game time. Those places
//! advance something per drawn frame by the logic's step. Seen in a mission: only 0x45ef16
//! (game_frame's call of 0x44d9f0, envelopes of a list at 0x661d6c whose results are not
//! stored: probably the Xbox version's rumble).

use super::{advapi32, frames, guest_call, log, mem, ticks};
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
pub const WORLD_TO_SCREEN: u32 = 0x5492a0;
pub const PROJECTION: u32 = 0x549cb0;
/// camera_apply's (0x40c2b0) call of the projection (its return address).
const CAMERA_PROJECTION: u32 = 0x40c35b;
/// Calls of world_to_screen (their return addresses) whose point is put between its ticks:
/// the target markers' two (0x446dc0) and the crosshair's target (0x43f160, the player's aim
/// point [[player+0x174]+0x3738]+0x10 to the HUD's +0xd10).
const SMOOTH_POINTS: [u32; 3] = [0x447001, 0x447010, 0x43f409];
pub const CROSSHAIR_DRAW: u32 = 0x43ffd0;
/// The crosshair moves half way to its target in this time (ms): the original's step at 60 fps.
pub const CROSSHAIR_HALF: f64 = 1000.0 / 60.0;
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

thread_local! {
    /// Depth of tick parts on this thread (the tick thread: 1 while it runs the world update).
    static IN_TICK: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// The tick thread runs a tick part (`tick`).
pub fn set_in_tick(n: u32) {
    IN_TICK.set(n);
}
static IN_FRAME: AtomicBool = AtomicBool::new(false);
static STEPS: Mutex<BTreeMap<u32, u32>> = Mutex::new(BTreeMap::new());
static FRAMES: AtomicU32 = AtomicU32::new(0);

/// The places drawn at one address at the last two ticks it was drawn at.
#[derive(Default)]
struct Track {
    /// places at the tick before (only those seen in the tick's first frame)
    prev: Vec<[f32; PLACE_N]>,
    /// places at this tick, and whether each was seen in the tick's first frame
    cur: Vec<([f32; PLACE_N], bool)>,
    prev_t: u32,
    cur_t: u32,
    /// the first frame that drew it at this tick
    first: u32,
    /// the frame it was last drawn in
    frame: u32,
}

/// At most this many places per address and tick.
const MAX_PLACES: usize = 4096;

/// The squared distance of two places' positions.
fn dist2(a: &[f32; PLACE_N], b: &[f32; PLACE_N]) -> f32 {
    (ROT..ROT + 3).map(|i| (a[i] - b[i]).powi(2)).sum()
}

/// The index of the place in `set` nearest to `p` (by position), if any.
fn nearest<'a>(p: &[f32; PLACE_N], set: impl Iterator<Item = &'a [f32; PLACE_N]>) -> Option<(usize, f32)> {
    set.enumerate().map(|(i, q)| (i, dist2(p, q))).min_by(|a, b| a.1.total_cmp(&b.1))
}

/// The place of the tick before that `now` continues: the nearest one of `prev`, when `now` is
/// also the nearest to it of this tick's places `cur` (those marked usable) and it did not jump.
pub fn partner(prev: &[[f32; PLACE_N]], cur: &[([f32; PLACE_N], bool)], now: &[f32; PLACE_N]) -> Option<[f32; PLACE_N]> {
    let (i, d) = nearest(now, prev.iter())?;
    if !(d.sqrt() < JUMP) {
        return None;
    }
    let p = &prev[i];
    let back = dist2(p, now);
    // another place of this tick nearer to it: that one continues it
    let taken = cur.iter().filter(|c| c.1 && c.0 != *now).any(|c| dist2(p, &c.0) < back);
    (!taken).then_some(*p)
}

#[derive(Default)]
struct State {
    tracks: HashMap<u32, Track>,
    /// the camera's last cam_set in this frame (position, rotation), if it was changed
    camera: Option<(u32, u32)>,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

/// The drawing's clock.
struct Clock {
    /// the host's clock (ms) minus the game's real clock: the smallest seen
    offset: Option<f64>,
    /// the frame and game time `alpha` was fixed for, and it
    fixed: Option<(u32, u32, f32)>,
}

static CLOCK: Mutex<Clock> = Mutex::new(Clock { offset: None, fixed: None });

/// More than this (ms) above the offset: the game's clock lost time (a pause), start again.
const OFFSET_RESET: f64 = 100.0;

/// The offset of the host's clock to the game's, given the last one and the one seen now
/// (`host - game`, at least the true offset: the game read its clock before). The smallest seen
/// is the true one up to the read's delay; one far larger means the game's clock lost time.
pub fn clock_offset(last: Option<f64>, seen: f64) -> f64 {
    match last {
        Some(k) if seen >= k && seen - k <= OFFSET_RESET => k,
        _ => seen,
    }
}

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
        s.camera = None;
    }
    let log = frames::on();
    if log {
        frames::frame_start();
    }
    if ticks::on() {
        ticks::frame();
    }
    super::tick::join();
    super::tick::frame();
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

/// Inside game_frame (main thread).
pub fn in_frame() -> bool {
    IN_FRAME.load(Ordering::Relaxed)
}

/// The game time of what is drawn: game time (+0x4dc), less one step while a tick runs on
/// the tick thread (its part 0 has moved the game time on, but A shows the tick before).
fn world_time(p: u32) -> u32 {
    let m = mem();
    let t = m.rd32(p + 0x4dc);
    if super::tick::inflight() { t.wrapping_sub(m.rd32(p + 0x4c8)) } else { t }
}

/// How far between the last two ticks to draw (0..1), or None: not now.
fn alpha() -> Option<f32> {
    if !smooth() || !IN_FRAME.load(Ordering::Relaxed) || IN_TICK.get() != 0 {
        return None;
    }
    let m = mem();
    let p = m.rd32(PROJ_DATA);
    let step = m.rd32(p + 0x4c8) as i32;
    if m.rd32(p + 0x4c4) == 0 || step <= 0 {
        return None;
    }
    let (real, game) = (m.rd32(p + 0x360), world_time(p));
    let frame = FRAMES.load(Ordering::Relaxed);
    let mut clock = CLOCK.lock().unwrap();
    if let Some((f, g, a)) = clock.fixed {
        if f == frame && g == game {
            return Some(a);
        }
    }
    let host = super::kernel32::start_time().elapsed().as_secs_f64() * 1000.0;
    let offset = clock_offset(clock.offset, host - real as f64);
    clock.offset = Some(offset);
    let ahead = host - offset - game as f64;
    let a = (ahead / step as f64).clamp(0.0, 1.0) as f32;
    clock.fixed = Some((frame, game, a));
    Some(a)
}

/// The place to draw for the value `now` at `key` (its address), or None: draw it as it is.
fn place(key: u32, now: &[f32; PLACE_N], n: usize, alpha: f32) -> Option<[f32; PLACE_N]> {
    let m = mem();
    let p = m.rd32(PROJ_DATA);
    let (t, step) = (world_time(p), m.rd32(p + 0x4c8));
    let frame = FRAMES.load(Ordering::Relaxed);
    let mut guard = STATE.lock().unwrap();
    let s = guard.get_or_insert_with(State::default);
    let tr = s.tracks.entry(key).or_insert_with(|| Track { prev_t: t.wrapping_sub(1), cur_t: t.wrapping_sub(1), ..Default::default() });
    if tr.cur_t != t {
        // a new tick: this tick's places become the tick before's
        tr.prev = tr.cur.drain(..).filter(|c| c.1).map(|c| c.0).collect();
        tr.prev_t = tr.cur_t;
        tr.cur_t = t;
        tr.first = frame;
    }
    tr.frame = frame;
    // compared on the floats this call uses
    let mut now_n = [0.0; PLACE_N];
    now_n[..n].copy_from_slice(&now[..n]);
    let first_frame = tr.first == frame;
    let known = tr.cur.iter().position(|c| c.0 == now_n);
    let usable = match known {
        Some(k) => tr.cur[k].1,
        None => {
            if tr.cur.len() < MAX_PLACES {
                tr.cur.push((now_n, first_frame));
            }
            first_frame
        }
    };
    if !usable || tr.prev_t.wrapping_add(step) != tr.cur_t {
        return None;
    }
    let prev = partner(&tr.prev, &tr.cur, &now_n)?;
    let mut prev_full = *now;
    prev_full[..n].copy_from_slice(&prev[..n]);
    Some(blend(&prev_full, now, n, alpha))
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
    if ticks::on() {
        ticks::camera(pos, rot);
    }
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

/// The projection (near, far, aspect * fov, fov): camera_apply's fov between its ticks.
pub fn projection(c: &mut Cpu) {
    let m = mem();
    let e = c.r[ESP];
    let Some(a) = alpha().filter(|_| m.rd32(e) == CAMERA_PROJECTION) else {
        return call_original(c, PROJECTION);
    };
    let (wide, fov) = (f32::from_bits(m.rd32(e + 12)), f32::from_bits(m.rd32(e + 16)));
    let mut now = [0.0; PLACE_N];
    now[ROT] = fov;
    let shown = place(CAMERA_PROJECTION, &now, ROT + 3, a).map_or(fov, |p| p[ROT]);
    if shown == fov || fov == 0.0 {
        return call_original(c, PROJECTION);
    }
    m.wr32(e + 12, (wide * shown / fov).to_bits());
    m.wr32(e + 16, shown.to_bits());
    call_original(c, PROJECTION);
}

/// The share of the way to its target that the crosshair moves in `dt` ms.
pub fn crosshair_share(dt: f64) -> f32 {
    (1.0 - 0.5f64.powf(dt.max(0.0) / CROSSHAIR_HALF)) as f32
}

/// When each crosshair_draw call (this, return address) last drew (host ms).
static CROSSHAIR_LAST: Mutex<Option<HashMap<(u32, u32), f64>>> = Mutex::new(None);

/// crosshair_draw(target, ..): `place += (target - place) * 0.5`, then drawn. `place` is set
/// before so that the game's half step is the share of the time since the last draw:
/// place' = place + (target - place) * share, so place before = 2 * place' - target.
pub fn crosshair_draw(c: &mut Cpu) {
    if alpha().is_none() {
        return call_original(c, CROSSHAIR_DRAW);
    };
    let m = mem();
    let (this, ret, at) = (c.r[ECX], m.rd32(c.r[ESP]), m.rd32(c.r[ESP] + 4));
    let host = super::kernel32::start_time().elapsed().as_secs_f64() * 1000.0;
    let dt = {
        let mut g = CROSSHAIR_LAST.lock().unwrap();
        let last = g.get_or_insert_with(HashMap::new).insert((this, ret), host);
        last.map_or(CROSSHAIR_HALF, |t| (host - t).min(1000.0))
    };
    let mut target = [0.0f32; 3];
    read(at, &mut target);
    let share = crosshair_share(dt);
    let cur = this + 0xd34;
    let mut place_now = [0.0f32; 3];
    read(cur, &mut place_now);
    let before: [f32; 3] = std::array::from_fn(|i| {
        let t = target[i];
        let next = place_now[i] + (t - place_now[i]) * share;
        2.0 * next - t
    });
    write(cur, &before);
    call_original(c, CROSSHAIR_DRAW);
}

/// world_to_screen(point, out): the crosshair's points between their ticks.
pub fn world_to_screen(c: &mut Cpu) {
    let m = mem();
    let (ret, at) = (m.rd32(c.r[ESP]), m.rd32(c.r[ESP] + 4));
    let Some(a) = alpha().filter(|_| SMOOTH_POINTS.contains(&ret)) else {
        return call_original(c, WORLD_TO_SCREEN);
    };
    let mut now = [0.0; PLACE_N];
    read(at, &mut now[ROT..ROT + 3]);
    // known by the call, not by the point's address (on the stack)
    let Some(shown) = place(ret, &now, ROT + 3, a) else {
        return call_original(c, WORLD_TO_SCREEN);
    };
    write(at, &shown[ROT..ROT + 3]);
    call_original(c, WORLD_TO_SCREEN);
    write(at, &now[ROT..ROT + 3]);
}

pub fn game_step_ms(c: &mut Cpu) {
    if finding() && IN_TICK.get() == 0 {
        let ret = mem().rd32(c.r[ESP]);
        *STEPS.lock().unwrap().entry(ret).or_default() += 1;
    }
    call_original(c, GAME_STEP_MS);
}

/// The main thread is inside a tick part.
pub fn in_tick() -> bool {
    IN_TICK.get() != 0
}

fn tick_part(c: &mut Cpu, i: usize) {
    // a tick still running on the tick thread ends before the next one's parts
    super::tick::join();
    let outer = IN_TICK.get() == 0;
    IN_TICK.set(IN_TICK.get() + 1);
    let aids = outer && ticks::on();
    if aids {
        ticks::part_start();
    }
    // the world update's this (0x413c60), for the fingerprint
    let world = c.r[ECX];
    let t = (outer && frames::on()).then(frames::part_start);
    // the world update on the tick thread when it can (`tick`)
    if !(i == TICK_PARTS.len() - 1 && super::tick::world_update(c)) {
        call_original(c, TICK_PARTS[i]);
    }
    IN_TICK.set(IN_TICK.get() - 1);
    if let Some(t) = t {
        frames::part_end(i, t);
    }
    // a tick on the tick thread ends at its join (`tick::join`)
    if aids && i == TICK_PARTS.len() - 1 && !super::tick::inflight() {
        ticks::tick_end(world);
    }
}

macro_rules! tick_parts {
    ($($i:literal),*) => { [$((TICK_PARTS[$i], (|c: &mut Cpu| tick_part(c, $i)) as crate::rt::Hook)),*] };
}

pub const HOOKS: [(u32, crate::rt::Hook); 16] = {
    let t = tick_parts!(0, 1, 2, 3, 4, 5, 6);
    [(GAME_FRAME, game_frame), (GAME_STEP_MS, game_step_ms), (CAM_SET, cam_set), (SET_WORLD, set_world), t[0], t[1], t[2], t[3], t[4], t[5], t[6], (EFFECTS_DRAW, effects_draw), (TRAIL_DRAW, trail_draw), (WORLD_TO_SCREEN, world_to_screen), (PROJECTION, projection), (CROSSHAIR_DRAW, crosshair_draw)]
};
