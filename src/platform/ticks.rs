//! Step 0 of the tick thread (docs/superpowers/plans/2026-10-31-tick-thread.md): measurements
//! that decide its details, and two aids for the later equivalence test. Nothing here changes
//! what the game computes.
//!
//! - `GUNMETAL_TICK_PRINT=<file>`: the logic fingerprint, one line per mission tick, written
//!   when the world update (0x413c60) returns: `print_line`. Values only, no addresses, so two
//!   runs can be compared line by line (`first_divergence`, `examples/tickprint.rs`).
//!   `GUNMETAL_TICK_PRINT_UNITS=<tick>[,<tick>]` adds the units' hashed values at those ticks,
//!   `GUNMETAL_TICK_PRINT_RAND=1` each tick's `_rand` callers in call order (`# ...` lines).
//! - Tick-indexed input: `GUNMETAL_INPUT` items `t<N>:<what>` are delivered at the start of
//!   mission tick N (`window::tick_inputs`), before the tick reads the keyboard and mouse.
//! - `GUNMETAL_TICK_PROBE=1`: counts and timings, logged at the end (ExitProcess): the C
//!   runtime's heap type (0x770ea8) at the mission's first tick, malloc / free and `_rand`
//!   inside and outside ticks (with `_rand`'s callers), the router's per-frame method
//!   (0x504360, vtable +0x4c of world+0x2c, called after the tick loop): its list and the
//!   methods it calls; GetWriteWatch with reset over the committed ranges only, and the
//!   committed size of the guest memory, every 25th mission tick.
//!
//! The mission's ticks are counted from the first tick in mission state (0x664728 = 0xe) on;
//! a tick is the first tick part after the last one's end up to 0x413c60's return.
//! Either variable installs the hooks of `HOOKS` (`platform::hooks()`, part of the compiled
//! code's cache key); unset, the tick parts' hooks cost one relaxed load more each.

use super::{lock, log, mem};
use crate::rt::{Cpu, ECX, ESP, call_original};
use std::collections::HashMap;
use std::io::Write;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

pub const RAND: u32 = 0x588042;
pub const MALLOC: u32 = 0x5888b4;
pub const FREE: u32 = 0x5887cb;
/// The router's method +0x4c, called once per frame after the tick loop.
pub const ROUTER_FRAME: u32 = 0x504360;
/// The C runtime's heap type (__active_heap): 1 system heap, 2 V5 small-block, 3 V6 small-block.
pub const ACTIVE_HEAP: u32 = 0x770ea8;
/// The game's state; 0xe in a mission (app_deactivate 0x461df0 tests it).
pub const GAME_STATE: u32 = 0x664728;
pub const PROJ_DATA: u32 = 0x72a458;

/// Containers of the id router (by vtable) that keep their objects in an array of slots at +4.
const SLOT_CONTAINERS: [(u32, u32); 3] = [(0x5e56e8, 512), (0x5e56ac, 1024), (0x5e7b84, 100)];
/// The units' container (id type 2).
const UNITS: u32 = 0x5e56e8;
/// The player's object: the one object of this class in the units' container (mission 1; its
/// place is not in it: the camera, which follows the player, stands for that).
const PLAYER_VT: u32 = 0x5e4bc8;
/// At most this many bytes of a unit are hashed (the size HeapAlloc was asked for bounds it).
const MAX_BYTES: u32 = 0x4000;

static ON: AtomicBool = AtomicBool::new(false);
static PROBE: AtomicBool = AtomicBool::new(false);
/// the camera's position and rotation (cam_set's arguments: the camera's +0x1cc, +0x1a8)
static CAM_POS: AtomicU32 = AtomicU32::new(0);
static CAM_ROT: AtomicU32 = AtomicU32::new(0);

/// Any of the tick aids is on.
#[inline(always)]
pub fn on() -> bool {
    ON.load(Ordering::Relaxed)
}

/// The hooks are wanted (either variable set): read at start by `platform::hooks()`.
pub fn hooks_wanted() -> bool {
    ["GUNMETAL_TICK_PRINT", "GUNMETAL_TICK_PROBE"].iter().any(|v| std::env::var_os(v).is_some_and(|s| !s.is_empty() && s != "0"))
}

pub const HOOKS: [(u32, crate::rt::Hook); 4] = [(RAND, rand), (MALLOC, malloc), (FREE, free), (ROUTER_FRAME, router_frame)];

// ---- the fingerprint ------------------------------------------------------------------------------

/// One mission tick's logic fingerprint.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Print {
    pub tick: u32,
    /// game time (proj_data+0x4dc), ms
    pub time: u32,
    /// the camera's position and 3x3 rotation
    pub camera: [f32; 12],
    /// the player's object: its slot in the units' container and a hash of its values
    pub player: Option<(u32, u32)>,
    /// live objects by id type, in the slot containers
    pub objects: Vec<(u8, u32)>,
    /// FNV-1a over every unit's slot, class and values
    pub units: u32,
    /// `_rand` calls in this tick, its seed after them, and a hash of where they came from
    pub rand: (u32, u32, u32),
}

fn floats(v: &[f32]) -> String {
    v.iter().map(|f| format!("{f:?}")).collect::<Vec<_>>().join(",")
}

/// `tick=N time=MS cam=x,y,z,r0..r8 player=slot:hash objects=2:14,5:3 units=hash
/// rand=calls:seed:callers`; floats in Rust's shortest exact form, so equal text means equal bits.
pub fn print_line(p: &Print) -> String {
    let player = p.player.map_or("-".to_string(), |(s, h)| format!("{s}:{h:08x}"));
    let objects = p.objects.iter().map(|(t, n)| format!("{t}:{n}")).collect::<Vec<_>>().join(",");
    format!(
        "tick={} time={} cam={} player={player} objects={} units={:08x} rand={}:{:08x}:{:08x}",
        p.tick,
        p.time,
        floats(&p.camera),
        if objects.is_empty() { "-" } else { &objects },
        p.units,
        p.rand.0,
        p.rand.1,
        p.rand.2
    )
}

/// The first line where two fingerprint files differ, and the first field (`key`) that does;
/// a missing line counts as a difference in "tick".
pub fn first_divergence(a: &[&str], b: &[&str]) -> Option<(usize, String)> {
    let fields = |l: &str| -> Vec<(String, String)> {
        l.split_whitespace().map(|f| f.split_once('=').map_or((f.to_string(), String::new()), |(k, v)| (k.to_string(), v.to_string()))).collect()
    };
    for i in 0..a.len().max(b.len()) {
        let (Some(x), Some(y)) = (a.get(i), b.get(i)) else { return Some((i, "tick".into())) };
        if x == y {
            continue;
        }
        let (fx, fy) = (fields(x), fields(y));
        for j in 0..fx.len().max(fy.len()) {
            match (fx.get(j), fy.get(j)) {
                (Some(p), Some(q)) if p == q => {}
                (Some(p), _) => return Some((i, p.0.clone())),
                (None, Some(q)) => return Some((i, q.0.clone())),
                (None, None) => {}
            }
        }
    }
    None
}

pub fn fnv(h: u32, w: u32) -> u32 {
    w.to_le_bytes().iter().fold(h, |h, &b| (h ^ b as u32).wrapping_mul(0x0100_0193))
}

// ---- state ----------------------------------------------------------------------------------------

#[derive(Default)]
struct Counts {
    rand_in: u64,
    rand_out: u64,
    rand_out_mission: u64,
    /// by (inside a tick, in a mission, return address)
    rand_callers: HashMap<(bool, bool, u32), u64>,
    malloc_in: u64,
    malloc_out: u64,
    free_in: u64,
    free_out: u64,
    /// mission ticks and pictures (game_frame calls in a mission)
    ticks: u64,
    frames: u64,
    router_calls: u64,
    router_time: Duration,
    router_nodes: u64,
    router_nodes_max: u64,
    router_frac_nonzero: u64,
    /// method +0x44 of the objects on the router's list, by address
    router_targets: HashMap<u32, u64>,
    /// GetWriteWatch and VirtualQuery measurements by name: (ms, pages or count)
    watch: std::collections::BTreeMap<&'static str, Vec<(f64, usize)>>,
    /// time in the fingerprint
    print_time: Duration,
}

struct Ticks {
    print: Option<std::io::BufWriter<std::fs::File>>,
    /// the mission's tick count (None outside a mission)
    tick: Option<u32>,
    /// a tick is open (its first part ran, 0x413c60 has not returned)
    open: bool,
    rand_tick: u32,
    seed: u32,
    /// FNV-1a over the return addresses of this tick's `_rand` calls, and (with
    /// `GUNMETAL_TICK_PRINT_RAND=1`) their return addresses in call order (runs counted)
    rand_from: u32,
    rand_list: bool,
    rand_callers_tick: Vec<(u32, u32)>,
    heap_logged: bool,
    /// `GUNMETAL_TICK_PRINT_UNITS=<tick>[,<tick>...]`: the units' values at those ticks into the
    /// file too
    units_at: Vec<u32>,
    c: Counts,
    #[cfg(windows)]
    watch: Vec<usize>,
}

static TICKS: Mutex<Option<Ticks>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut Ticks) -> R) -> Option<R> {
    lock(&TICKS).as_mut().map(f)
}

/// Called at start (run_game): turns the aids on that the environment asks for.
pub fn init() {
    let print = std::env::var_os("GUNMETAL_TICK_PRINT").filter(|p| !p.is_empty());
    let probe = std::env::var_os("GUNMETAL_TICK_PROBE").is_some_and(|v| !v.is_empty() && v != "0");
    // tick-indexed input, played or recorded
    let input = super::window::has_tick_inputs() || super::record::on();
    if print.is_none() && !probe && !input {
        return;
    }
    let out = print.as_ref().map(|p| std::io::BufWriter::new(std::fs::File::create(p).expect("GUNMETAL_TICK_PRINT file")));
    *lock(&TICKS) = Some(Ticks {
        print: out,
        tick: None,
        open: false,
        rand_tick: 0,
        seed: 0,
        rand_from: 0,
        rand_list: std::env::var_os("GUNMETAL_TICK_PRINT_RAND").is_some_and(|v| v != "0"),
        rand_callers_tick: Default::default(),
        heap_logged: false,
        units_at: std::env::var("GUNMETAL_TICK_PRINT_UNITS").map(|v| v.split(',').filter_map(|t| t.parse().ok()).collect()).unwrap_or_default(),
        c: Counts::default(),
        #[cfg(windows)]
        watch: if probe { vec![0; 1 << 20] } else { Vec::new() },
    });
    PROBE.store(probe, Ordering::Relaxed);
    ON.store(true, Ordering::Relaxed);
    if let Some(p) = print {
        log!("ticks: fingerprints to {}", std::path::Path::new(&p).display());
    }
}

/// The mission tick the game is at (the last one started), when the tick aids are on and a
/// mission runs.
pub fn mission_tick() -> Option<u32> {
    if !on() || !in_mission() {
        return None;
    }
    with(|s| s.tick).flatten()
}

fn in_mission() -> bool {
    mem().rd32(GAME_STATE) == 0xe
}

/// The sizes HeapAlloc was asked for, by block (our blocks are larger by how the free space
/// was split, which depends on the order of allocations): what of a unit is hashed.
static ASKED: Mutex<Option<HashMap<u32, u32>>> = Mutex::new(None);

/// HeapAlloc gave `p` for `n` bytes (`n` 0: `p` was freed).
pub fn allocated(p: u32, n: u32) {
    let mut a = lock(&ASKED);
    let a = a.get_or_insert_with(HashMap::new);
    if n == 0 {
        a.remove(&p);
    } else if p != 0 {
        a.insert(p, n);
    }
}

fn asked(p: u32) -> u32 {
    lock(&ASKED).as_ref().and_then(|a| a.get(&p).copied()).unwrap_or(0) & !3
}

/// The camera's place for the fingerprint (cam_set's arguments).
pub fn camera(pos: u32, rot: u32) {
    CAM_POS.store(pos, Ordering::Relaxed);
    CAM_ROT.store(rot, Ordering::Relaxed);
}

/// A tick part starts (outermost): the first one after a tick's end opens a tick; tick-indexed
/// input for it is delivered now, before part 1 reads the keyboard.
pub fn part_start() {
    let mission = in_mission();
    let due = with(|s| {
        if s.open {
            return None;
        }
        s.open = true;
        s.rand_tick = 0;
        s.rand_from = 0x811c_9dc5;
        s.rand_callers_tick.clear();
        if !mission {
            s.tick = None;
            return None;
        }
        let t = s.tick.map_or(0, |t| t + 1);
        s.tick = Some(t);
        s.c.ticks += 1;
        if !s.heap_logged {
            s.heap_logged = true;
            let h = mem().rd32(ACTIVE_HEAP);
            log!("ticks: mission tick 0 at {} ms: C runtime heap type (0x770ea8) {h} ({})", super::kernel32::ms_since_start(), match h {
                1 => "system heap: malloc is HeapAlloc",
                2 => "V5 small-block heap",
                3 => "V6 small-block heap",
                _ => "?",
            });
        }
        Some(t)
    })
    .flatten();
    if let Some(t) = due {
        for i in super::window::tick_inputs(t as u64) {
            use super::window::Input;
            match i {
                Input::Poke(a, v) => mem().wr32(a, v),
                Input::LoseDevice(ms) => super::d3d9::device::simulate_loss(Duration::from_millis(ms as u64)),
                Input::Stall(ms) => std::thread::sleep(Duration::from_millis(ms as u64)),
                Input::Minimized(_) => log!("ticks: minimize/restore are not tick-indexed (ignored)"),
                i => super::user32::feed(i, true),
            }
        }
    }
}

/// 0x413c60 (the world update, ecx `world`) returned: the tick ends.
pub fn tick_end(world: u32) {
    let probe = PROBE.load(Ordering::Relaxed);
    let mut watch_now = false;
    let line = with(|s| {
        s.open = false;
        let t = s.tick?;
        watch_now = probe && t % 25 == 24;
        let t0 = Instant::now();
        let p = s.print.is_some().then(|| fingerprint(t, world, (s.rand_tick, s.seed, s.rand_from)));
        let mut l = p.map(|p| print_line(&p));
        if s.rand_list {
            if let Some(l) = l.as_mut() {
                let v: Vec<String> = s.rand_callers_tick.iter().map(|(a, n)| if *n == 1 { format!("{a:x}") } else { format!("{a:x}*{n}") }).collect();
                l.push_str(&format!("\n# tick {t} rand {}", v.join(" ")));
            }
        }
        if s.units_at.contains(&t) {
            // the values behind the units' hash, one line each (finding what differs)
            if let Some(l) = l.as_mut() {
                for (slot, vt, off, v) in unit_values(world) {
                    l.push_str(&format!("
# tick {t} unit {slot} class {vt:#x} +{off:#x} {v:08x}"));
                }
            }
        }
        s.c.print_time += t0.elapsed();
        l
    })
    .flatten();
    if let Some(l) = line {
        with(|s| {
            if let Some(o) = s.print.as_mut() {
                let _ = writeln!(o, "{l}");
            }
        });
    }
    if watch_now {
        measure_watch();
    }
    if let Some(t) = with(|s| s.tick).flatten().filter(|&t| probe && t == 100) {
        dump(world, t);
    }
}

/// At mission tick 100: the camera, the router's containers and the classes in the units'
/// container (for the log).
fn dump(world: u32, tick: u32) {
    let m = mem();
    let (cp, cr) = (CAM_POS.load(Ordering::Relaxed), CAM_ROT.load(Ordering::Relaxed));
    if cp != 0 && readable(cp - 0x1cc, 0x1d8) {
        let cam: Vec<f32> = (0..3).map(|i| f32::from_bits(m.rd32(cp + 4 * i))).collect();
        log!("ticks: tick {tick}: camera object vtable {:#x} (pos - 0x1cc), rotation at pos - {:#x}, position {cam:?}", m.rd32(cp - 0x1cc), cp - cr);
    }
    let mut hs = handlers(world);
    hs.sort();
    for (key, h) in hs {
        let vt = m.rd32(h);
        let n = SLOT_CONTAINERS.iter().find(|c| c.0 == vt).map(|c| c.1);
        let slots = m.rd32(h + 4);
        let ok = n.is_some_and(|n| readable(slots, 4 * n));
        let live = n.filter(|_| ok).map(|n| (0..n).filter(|&i| m.rd32(slots + 4 * i) != 0).count());
        log!("ticks: tick {tick}: router handler key {key:#x} vtable {vt:#x} slots {n:?} live {live:?}");
        if vt == UNITS && ok {
            let mut by_class: std::collections::BTreeMap<u32, u32> = Default::default();
            for i in 0..512u32 {
                let o = m.rd32(slots + 4 * i);
                if o != 0 && readable(o, 4) {
                    *by_class.entry(m.rd32(o)).or_default() += 1;
                }
            }
            log!("ticks: tick {tick}: units by class (vtable count): {by_class:x?}");
        }
    }
}

/// game_frame starts (for the picture count).
pub fn frame() {
    if in_mission() {
        with(|s| s.c.frames += 1);
    }
}

/// [a, a + n) lies in the image or the heap's mapped range: a cheap check for pointers read
/// from the game's objects (`guest_readable`'s VirtualQuery costs ~0.25 ms in a mission: it
/// scans the reservation's pages).
fn readable(a: u32, n: u32) -> bool {
    let (lo, hi) = super::heap_range();
    let end = a as u64 + n as u64;
    (a >= 0x40_0000 && end <= 0x77_3000) || (a >= lo && end <= hi as u64)
}

/// FNV-1a over the dwords of [a, a + n) that are values, not pointers: dwords inside the heap
/// or the stacks are left out (as floats those are below 1e-13; heap addresses depend on the
/// order of allocations on several threads).
fn values(mut h: u32, a: u32, n: u32) -> u32 {
    let m = mem();
    let (lo, hi) = super::heap_range();
    for k in (0..n).step_by(4) {
        let v = m.rd32(a + k);
        if (lo..hi).contains(&v) || (super::STACK_LO..super::STACK_HI).contains(&v) {
            continue;
        }
        h = fnv(h, v);
    }
    h
}

fn fingerprint(tick: u32, world: u32, rand: (u32, u32, u32)) -> Print {
    let m = mem();
    let p = m.rd32(PROJ_DATA);
    let time = if p != 0 { m.rd32(p + 0x4dc) } else { 0 };
    let (cp, cr) = (CAM_POS.load(Ordering::Relaxed), CAM_ROT.load(Ordering::Relaxed));
    let mut camera = [0.0; 12];
    if cp != 0 && readable(cp, 12) && readable(cr, 36) {
        for i in 0..3 {
            camera[i] = f32::from_bits(m.rd32(cp + 4 * i as u32));
        }
        for i in 0..9 {
            camera[3 + i] = f32::from_bits(m.rd32(cr + 4 * i as u32));
        }
    }
    let mut objects = Vec::new();
    let mut units = 0x811c_9dc5u32;
    let mut player = None;
    for (key, h) in handlers(world) {
        let vt = m.rd32(h);
        let Some(&(_, n)) = SLOT_CONTAINERS.iter().find(|c| c.0 == vt) else { continue };
        let slots = m.rd32(h + 4);
        if slots == 0 || !readable(slots, 4 * n) {
            continue;
        }
        let mut live = 0;
        for i in 0..n {
            let o = m.rd32(slots + 4 * i);
            if o == 0 || !readable(o, 4) {
                continue;
            }
            live += 1;
            let size = asked(o).min(MAX_BYTES);
            if vt == UNITS && readable(o, size) {
                // which class lives in which slot (vtables are the EXE's, not heap addresses)
                let ovt = m.rd32(o);
                units = values(fnv(fnv(units, i), ovt), o, size);
                if ovt == PLAYER_VT && player.is_none() {
                    player = Some((i, values(0x811c_9dc5, o, size)));
                }
            }
        }
        objects.push(((key >> 24) as u8, live));
    }
    objects.sort();
    Print { tick, time, camera, player, objects, units, rand }
}

/// Every unit's hashed dwords: (slot, class, offset, value).
fn unit_values(world: u32) -> Vec<(u32, u32, u32, u32)> {
    let m = mem();
    let (lo, hi) = super::heap_range();
    let mut out = Vec::new();
    for (_, h) in handlers(world) {
        if m.rd32(h) != UNITS || !readable(m.rd32(h + 4), 4 * 512) {
            continue;
        }
        let slots = m.rd32(h + 4);
        for i in 0..512 {
            let o = m.rd32(slots + 4 * i);
            let n = asked(o).min(MAX_BYTES);
            if o == 0 || !readable(o, n) {
                continue;
            }
            let vt = m.rd32(o);
            for k in (0..n).step_by(4) {
                let v = m.rd32(o + k);
                if !(lo..hi).contains(&v) && !(super::STACK_LO..super::STACK_HI).contains(&v) {
                    out.push((i, vt, k, v));
                }
            }
        }
    }
    out
}

/// The router's handlers (id containers): (key, handler), from its tree at +0x9084.
fn handlers(world: u32) -> Vec<(u32, u32)> {
    let m = mem();
    let mut out = Vec::new();
    if world == 0 || !readable(world + 0x2c, 4) {
        return out;
    }
    let router = m.rd32(world + 0x2c);
    if router == 0 || !readable(router + 0x9084, 4) {
        return out;
    }
    let mut stack = vec![m.rd32(router + 0x9084)];
    while let Some(n) = stack.pop() {
        if n == 0 || out.len() > 64 || !readable(n, 0x14) {
            continue;
        }
        out.push((m.rd32(n + 8), m.rd32(n)));
        stack.push(m.rd32(n + 0xc));
        stack.push(m.rd32(n + 0x10));
    }
    out
}

// ---- hooks ----------------------------------------------------------------------------------------

fn rand(c: &mut Cpu) {
    let ret = mem().rd32(c.r[ESP]);
    call_original(c, RAND);
    // _rand leaves its new seed in ecx
    let (seed, tick) = (c.r[ECX], super::fps::in_tick());
    let mission = PROBE.load(Ordering::Relaxed) && in_mission();
    with(|s| {
        s.seed = seed;
        if tick {
            s.rand_tick += 1;
            s.rand_from = fnv(s.rand_from, ret);
            if s.rand_list {
                // in call order, runs of the same caller counted
                match s.rand_callers_tick.last_mut() {
                    Some(r) if r.0 == ret => r.1 += 1,
                    _ => s.rand_callers_tick.push((ret, 1)),
                }
            }
            s.c.rand_in += 1;
        } else {
            s.c.rand_out += 1;
            if mission {
                s.c.rand_out_mission += 1;
            }
        }
        if PROBE.load(Ordering::Relaxed) {
            *s.c.rand_callers.entry((tick, mission, ret)).or_default() += 1;
        }
    });
}

fn malloc(c: &mut Cpu) {
    if PROBE.load(Ordering::Relaxed) {
        let t = super::fps::in_tick();
        with(|s| if t { s.c.malloc_in += 1 } else { s.c.malloc_out += 1 });
    }
    call_original(c, MALLOC);
}

fn free(c: &mut Cpu) {
    if PROBE.load(Ordering::Relaxed) {
        let t = super::fps::in_tick();
        with(|s| if t { s.c.free_in += 1 } else { s.c.free_out += 1 });
    }
    call_original(c, FREE);
}

/// 0x504360 (thiscall on the router): +0x90a4 = (game time - the last tick's, +0x909c) * 0.03,
/// then each object on the list at [+0x9050] gets its method +0x44 with that.
fn router_frame(c: &mut Cpu) {
    if !PROBE.load(Ordering::Relaxed) || !in_mission() {
        return call_original(c, ROUTER_FRAME);
    }
    let m = mem();
    let this = c.r[ECX];
    let list = m.rd32(this + 0x9050);
    let mut targets = Vec::new();
    if list != 0 && m.rd32(list + 0xc) != 0 {
        let mut n = m.rd32(list + 4);
        while n != 0 && targets.len() < 100_000 {
            let o = m.rd32(n);
            targets.push(if o != 0 { m.rd32(m.rd32(o) + 0x44) } else { 0 });
            n = m.rd32(n + 4);
        }
    }
    let t = Instant::now();
    call_original(c, ROUTER_FRAME);
    let d = t.elapsed();
    let frac = m.rd32(this + 0x90a4);
    with(|s| {
        s.c.router_calls += 1;
        s.c.router_time += d;
        s.c.router_nodes += targets.len() as u64;
        s.c.router_nodes_max = s.c.router_nodes_max.max(targets.len() as u64);
        if f32::from_bits(frac) != 0.0 {
            s.c.router_frac_nonzero += 1;
        }
        for a in targets {
            *s.c.router_targets.entry(a).or_default() += 1;
        }
    });
}

// ---- write watch over the committed ranges --------------------------------------------------------

/// The committed ranges of the guest memory (guest addresses), by VirtualQuery.
#[cfg(windows)]
pub fn committed() -> Vec<(u32, u32)> {
    use windows_sys::Win32::System::Memory::{MEM_COMMIT, MEMORY_BASIC_INFORMATION, VirtualQuery};
    let base = mem().base as u64;
    let mut out: Vec<(u32, u32)> = Vec::new();
    let mut at = 0u64;
    while at < 1 << 32 {
        let mut mbi: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
        if unsafe { VirtualQuery((base + at) as _, &mut mbi, size_of::<MEMORY_BASIC_INFORMATION>()) } == 0 {
            break;
        }
        let lo = mbi.BaseAddress as u64 - base;
        let hi = (lo + mbi.RegionSize as u64).min(1 << 32);
        if mbi.State == MEM_COMMIT {
            match out.last_mut() {
                Some(r) if r.1 as u64 == lo => r.1 = hi as u32,
                _ => out.push((lo as u32, hi as u32)),
            }
        }
        at = hi;
    }
    out
}

/// GetWriteWatch over each range (`reset`: WRITE_WATCH_FLAG_RESET); pages found.
#[cfg(windows)]
fn watch_ranges(buf: &mut [usize], ranges: &[(u32, u32)], reset: bool) -> usize {
    use windows_sys::Win32::System::Memory::GetWriteWatch;
    let base = mem().base as usize;
    let mut pages = 0;
    for &(lo, hi) in ranges {
        let mut count = buf.len();
        let mut gran = 0u32;
        let r = unsafe { GetWriteWatch(reset as u32, (base + lo as usize) as _, (hi - lo) as usize, buf.as_mut_ptr() as _, &mut count, &mut gran) };
        if r == 0 {
            pages += count;
        }
    }
    pages
}

/// Every 25th mission tick: the committed ranges (VirtualQuery) and GetWriteWatch in several
/// forms, each timed: over the committed ranges with reset (the pages of ~25 ticks), the same
/// again (no pages: the scan alone), without reset, over the heap's range alone, over the image
/// alone, and over the whole reservation.
#[cfg(windows)]
fn measure_watch() {
    let mut buf = with(|s| std::mem::take(&mut s.watch)).unwrap_or_default();
    let mut out: Vec<(&'static str, f64, usize)> = Vec::new();
    let t = Instant::now();
    let ranges = committed();
    out.push(("VirtualQuery walk", t.elapsed().as_secs_f64() * 1e3, ranges.len()));
    let mb = ranges.iter().map(|r| (r.1 - r.0) as u64).sum::<u64>() >> 20;
    out.push(("committed MB", 0.0, mb as usize));
    let (lo, hi) = super::heap_range();
    let heap: Vec<(u32, u32)> = ranges.iter().filter(|r| r.0 < hi && lo < r.1).copied().collect();
    let image: Vec<(u32, u32)> = ranges.iter().filter(|r| r.0 < 0x80_0000 && 0x40_0000 < r.1).copied().collect();
    let mut timed = |name: &'static str, ranges: &[(u32, u32)], reset: bool| {
        let t = Instant::now();
        let n = watch_ranges(&mut buf, ranges, reset);
        out.push((name, t.elapsed().as_secs_f64() * 1e3, n));
    };
    timed("committed ranges, reset (pages of ~25 ticks)", &ranges, true);
    timed("committed ranges, reset again (no pages)", &ranges, true);
    timed("committed ranges, no reset (no pages)", &ranges, false);
    timed("heap arena's committed range alone, reset", &heap, true);
    timed("image's committed range alone, reset", &image, true);
    timed("whole 4 GB reservation, reset", &[(0, u32::MAX & !0xfff)], true);
    with(|s| {
        for (name, ms, n) in out {
            s.c.watch.entry(name).or_default().push((ms, n));
        }
        s.watch = buf;
    });
}

#[cfg(not(windows))]
fn measure_watch() {}

// ---- the end --------------------------------------------------------------------------------------

/// The end of the run (ExitProcess): the probe's findings.
pub fn finish() {
    if !on() {
        return;
    }
    ON.store(false, Ordering::Relaxed);
    let Some(mut s) = lock(&TICKS).take() else { return };
    if let Some(o) = s.print.as_mut() {
        let _ = o.flush();
    }
    if !PROBE.load(Ordering::Relaxed) {
        return;
    }
    let c = &s.c;
    let (t, f) = (c.ticks.max(1) as f64, c.frames.max(1) as f64);
    log!("ticks: probe over {} mission ticks and {} mission frames (whole run's counts per mission tick / frame)", c.ticks, c.frames);
    log!("ticks: malloc {} in ticks ({:.1}/tick), {} outside ({:.1}/frame); free {} in ticks ({:.1}/tick), {} outside ({:.1}/frame)", c.malloc_in, c.malloc_in as f64 / t, c.malloc_out, c.malloc_out as f64 / f, c.free_in, c.free_in as f64 / t, c.free_out, c.free_out as f64 / f);
    log!("ticks: _rand {} in ticks ({:.2}/tick), {} outside ({} of them in mission frames: {:.2}/frame)", c.rand_in, c.rand_in as f64 / t, c.rand_out, c.rand_out_mission, c.rand_out_mission as f64 / f);
    for (inside, mission, what) in [(true, true, "in mission ticks"), (false, true, "in a mission outside ticks"), (false, false, "outside missions")] {
        let mut v: Vec<(u32, u64)> = c.rand_callers.iter().filter(|e| e.0.0 == inside && e.0.1 == mission).map(|e| (e.0.2, *e.1)).collect();
        v.sort_by(|a, b| b.1.cmp(&a.1));
        let l: Vec<String> = v.iter().take(30).map(|(a, n)| format!("{a:#x} {n}")).collect();
        log!("ticks: _rand callers {what} (return address, calls): {}", l.join(", "));
    }
    log!(
        "ticks: router per-frame 0x504360: {} calls, avg {:.4} ms, list avg {:.1} objects (max {}), fraction != 0 in {} calls",
        c.router_calls,
        c.router_time.as_secs_f64() * 1e3 / c.router_calls.max(1) as f64,
        c.router_nodes as f64 / c.router_calls.max(1) as f64,
        c.router_nodes_max,
        c.router_frac_nonzero
    );
    let mut v: Vec<(u32, u64)> = c.router_targets.iter().map(|e| (*e.0, *e.1)).collect();
    v.sort_by(|a, b| b.1.cmp(&a.1));
    let l: Vec<String> = v.iter().map(|(a, n)| format!("{} {:.1}", crate::image::named(*a), *n as f64 / c.router_calls.max(1) as f64)).collect();
    log!("ticks: router per-frame: method +0x44 called (per frame): {}", l.join(", "));
    for (name, v) in &c.watch {
        let ms: Vec<f64> = v.iter().map(|e| e.0).collect();
        let n: Vec<f64> = v.iter().map(|e| e.1 as f64).collect();
        let (a, b) = (super::frames::stats(&ms), super::frames::stats(&n));
        log!("ticks: write watch, {name}: {} x avg {:.3} p50 {:.3} max {:.3} ms; pages/count avg {:.0} max {:.0}", a.n, a.avg, super::frames::percentile(&ms, 50), a.max, b.avg, b.max);
    }
    log!("ticks: fingerprints took {:.3} ms per tick", c.print_time.as_secs_f64() * 1e3 / t);
}
