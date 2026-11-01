//! The world update (0x413c60, "P6", 96 % of a logic tick) on a second host thread, in world
//! B (`world`; plan docs/superpowers/plans/2026-10-31-tick-thread.md). game_frame's tick loop
//! calls parts 0-5 on the main thread as before; the hook of P6 (`fps::tick_part`) calls
//! `world_update`, which forks: B and C are brought up to A, the main thread's stack is
//! copied, and the tick thread runs P6 from a copy of the main thread's registers with B as
//! its memory, posing as the main thread (thread id, TLS, last error). At the join its pages
//! are merged into A (`world::merge`), the frees it held back are done and the Direct3D
//! setters it recorded are made on the main thread.
//!
//! Modes (`world::mode`): `serial` waits for the tick at once (the machinery without
//! overlap); `1` lets the main thread go on (the rest of game_frame and its drawing) while the
//! tick runs, joined at the next game_frame, before any tick part, or before an import the
//! main thread calls outside game_frame; `check` as `1`, logging the bytes both changed.
//!
//! When: in a mission (variableUpdate on) after `STEADY` frames in a row, with the loader
//! thread (0x4eadf0) ended and the device not lost; otherwise P6 runs on the main thread as
//! before.

use super::{STACK_HI, STACK_LO, crash, free, game_mem, log, replay_calls, world};
use crate::rt::{Cpu, ESP, call_original};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Mutex, OnceLock};

pub const WORLD_UPDATE: u32 = 0x413c60;
/// The mission loader thread's start: no tick on the second thread while it runs.
const LOADER: u32 = 0x4eadf0;
/// Nonzero while the game's device is lost.
const DEVICE_LOST: u32 = 0x76d51d;
/// Mission frames in a row before the first fork.
const STEADY: u32 = 60;

static STEADY_FRAMES: AtomicU32 = AtomicU32::new(0);

/// A game_frame starts (main thread): count the mission frames in a row.
pub fn frame() {
    let m = game_mem();
    let p = m.rd32(super::fps::PROJ_DATA);
    let mission = p != 0 && m.rd32(p + 0x4c4) != 0 && m.rd32(p + 0x4c8) as i32 > 0;
    if mission {
        STEADY_FRAMES.fetch_add(1, Ordering::Relaxed);
    } else {
        STEADY_FRAMES.store(0, Ordering::Relaxed);
    }
}

fn eligible() -> bool {
    world::worlds().is_some()
        && STEADY_FRAMES.load(Ordering::Relaxed) >= STEADY
        && !crash::running(LOADER)
        && game_mem().rd8(DEVICE_LOST) == 0
}

/// What the tick thread gets: the registers at the fork (memory B) and who to pose as.
struct Job {
    cpu: Box<Cpu>,
    tid: u32,
    tls: Vec<u32>,
    last_error: u32,
}

struct Worker {
    jobs: Sender<Job>,
    done: Mutex<Receiver<Box<Cpu>>>,
}

fn worker() -> &'static Worker {
    static W: OnceLock<Worker> = OnceLock::new();
    W.get_or_init(|| {
        let (jobs, rx) = channel::<Job>();
        let (tx, done) = channel();
        let b = world::worlds().expect("worlds").b;
        std::thread::Builder::new()
            .name("tick".into())
            // guest calls nest on the host stack
            .stack_size(256 << 20)
            .spawn(move || {
                world::set_role(world::Role::Tick);
                super::set_world(Some(b));
                let mut registered = false;
                for mut job in rx {
                    super::kernel32::pose_as(job.tid, job.tls);
                    super::LAST_ERROR.set(job.last_error);
                    if !registered {
                        crash::register(crash::Kind::Tick, WORLD_UPDATE, job.cpu.fs_base);
                        registered = true;
                    }
                    super::fps::set_in_tick(1);
                    call_original(&mut job.cpu, WORLD_UPDATE);
                    super::fps::set_in_tick(0);
                    if tx.send(job.cpu).is_err() {
                        break;
                    }
                }
            })
            .expect("cannot start the tick thread");
        Worker { jobs, done: Mutex::new(done) }
    })
}

static FIRST: std::sync::Once = std::sync::Once::new();

/// A tick running on the tick thread (from its fork to its join).
struct Flight {
    /// the main thread's esp at the fork (its stack below is not merged)
    esp: u32,
    ranges: Vec<(usize, usize)>,
    /// P6's this (for the tick fingerprint at the join)
    world: u32,
    fcw: u16,
    synced: usize,
    fork: std::time::Duration,
}

static INFLIGHT: AtomicBool = AtomicBool::new(false);
static FLIGHT: Mutex<Option<Flight>> = Mutex::new(None);

/// Whether a tick runs on the tick thread now.
pub fn inflight() -> bool {
    INFLIGHT.load(Ordering::Acquire)
}

/// P6 called by game_frame's tick loop (its return address and argument on the stack, this
/// in ecx): run it on the tick thread. False when not now: the caller runs it as usual.
/// `serial` waits for it here; otherwise it returns as P6's `ret 4` would and the tick runs
/// on while the main thread draws, until `join`.
pub fn world_update(c: &mut Cpu) -> bool {
    join();
    if !eligible() {
        return false;
    }
    let w = world::worlds().expect("worlds");
    let ranges = world::page_ranges(w.a);
    let t0 = std::time::Instant::now();
    // fork: B and C equal A (the main thread's stack is not marked: copied whole)
    let mut first = false;
    FIRST.call_once(|| {
        world::initial(w);
        first = true;
    });
    if first {
        log!("tick: first fork at {} ms ({:?})", super::kernel32::ms_since_start(), world::mode(super::game_dir()));
    }
    let synced = if first { 0 } else { world::sync(w, &ranges) };
    let esp = c.r[ESP];
    world::copy_range(w.a, &[w.b, w.c], esp & !0xfff, STACK_HI);
    world::FREES.set_running(true);
    let mut cpu = Box::new(c.clone());
    cpu.mem = w.b.base;
    let job = Job { cpu, tid: super::kernel32::thread_id(), tls: super::kernel32::tls_values(), last_error: super::LAST_ERROR.get() };
    let wk = worker();
    *FLIGHT.lock().unwrap_or_else(|e| e.into_inner()) =
        Some(Flight { esp, ranges, world: c.r[crate::rt::ECX], fcw: c.fcw, synced, fork: t0.elapsed() });
    INFLIGHT.store(true, Ordering::Release);
    wk.jobs.send(job).expect("tick thread");
    if world::mode(super::game_dir()) == world::Mode::Serial {
        // the registers as P6 left them (ret 4 popped), memory A
        let done = join().expect("the tick just forked");
        let mem = c.mem;
        *c = (*done).clone();
        c.mem = mem;
    } else {
        // as P6's `ret 4`: the loop ignores eax, P6 keeps the other registers it must
        c.r[ESP] += 8;
    }
    true
}

/// Wait for the running tick, if any, and bring it into A (main thread): its pages merged
/// (not the main thread's stack below the fork), the frees it held back done, its Direct3D
/// setters made. Returns its registers at the end.
pub fn join() -> Option<Box<Cpu>> {
    if !inflight() {
        return None;
    }
    let f = FLIGHT.lock().unwrap_or_else(|e| e.into_inner()).take()?;
    let t0 = std::time::Instant::now();
    let wk = worker();
    let done = super::without_turn_result(|| wk.done.lock().unwrap_or_else(|e| e.into_inner()).recv().expect("tick thread"));
    let waited = t0.elapsed();
    let t1 = std::time::Instant::now();
    let w = world::worlds().expect("worlds");
    // the pool's globals are A's alone (`pool_alloc`): changed in B, the merge would bring a
    // second free list into A (seen: nodes handed out twice, a list walked for ever)
    for a in [POOL_HEAD, POOL_USED, POOL_BASE] {
        if w.b.rd32(a) != w.c.rd32(a) {
            static ONCE: std::sync::Once = std::sync::Once::new();
            ONCE.call_once(|| log!("tick: the pool's {a:#x} changed in B: {:#x} -> {:#x}", w.c.rd32(a), w.b.rd32(a)));
        }
    }
    let lists = lists_before((STACK_LO, f.esp));
    let merged = world::merge(w, &f.ranges, (STACK_LO, f.esp));
    lists_merge(&lists);
    world::FREES.set_running(false);
    for p in world::FREES.take() {
        free(p);
    }
    pool_frees();
    replay_calls(&world::take_calls());
    INFLIGHT.store(false, Ordering::Release);
    if done.fcw != f.fcw {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| log!("tick: the world update changed the FPU control word {:#x} -> {:#x} (not given to the main thread)", f.fcw, done.fcw));
    }
    if super::ticks::on() && world::mode(super::game_dir()) != world::Mode::Serial {
        super::ticks::tick_end(f.world);
    }
    if world::mode(super::game_dir()) == world::Mode::Check {
        static EARLY: AtomicU32 = AtomicU32::new(0);
        if EARLY.fetch_add(1, Ordering::Relaxed) < 20 && !merged.at.is_empty() {
            let list: Vec<String> = merged.at.iter().map(|a| format!("{a:#x}")).collect();
            log!("tick: join: {} pages copied, {} merged, {} bytes both changed: {}", merged.copied, merged.merged, merged.conflicts, list.join(" "));
        }
    }
    note(f.synced, f.fork, waited, t1.elapsed(), merged);
    Some(done)
}

// ---- the game's pool of 12-byte objects ---------------------------------------------------------

/// pool_alloc(size) (cdecl): a 12-byte object from the game's own pool (1.5 million nodes at
/// [0x76c2b8], free list head 0x76c2d4, count used 0x76c2b4, guarded by the critical section
/// 0x76c2bc), other sizes from operator new. pool_free(p) (cdecl) puts a node back.
pub const POOL_ALLOC: u32 = 0x55e5b0;
pub const POOL_FREE: u32 = 0x55e670;
const POOL_HEAD: u32 = 0x76c2d4;
const POOL_USED: u32 = 0x76c2b4;
/// The pool's nodes (0 before it is made and after 0x55e570 freed it at the exit).
const POOL_BASE: u32 = 0x76c2b8;
const POOL_BYTES: u32 = 0x112a880;

/// The tick and the drawing both take nodes: with a free list in each world they took the same
/// node (seen: a fault in pool_alloc at the first overlapped tick). So both use A's list, under
/// one lock, and a node given back while a tick runs is put on the list after the join (as the
/// heap's frees, `world::FREES`): the other world may still use it.
static POOL: Mutex<Vec<u32>> = Mutex::new(Vec::new());

fn pool_alloc(c: &mut Cpu) {
    let m = game_mem();
    // the argument on this thread's stack (the tick thread's is in B)
    let size = super::mem().rd32(c.r[ESP] + 4);
    let _g = POOL.lock().unwrap_or_else(|e| e.into_inner());
    let head = m.rd32(POOL_HEAD);
    if size != 12 || head == 0 || m.rd32(POOL_BASE) == 0 {
        // operator new, or the original's report of an empty pool
        drop(_g);
        return call_original(c, POOL_ALLOC);
    }
    m.wr32(POOL_HEAD, m.rd32(head + 4));
    m.wr32(POOL_USED, m.rd32(POOL_USED).wrapping_add(1));
    c.r[crate::rt::EAX] = head;
    c.r[ESP] += 4;
}

fn pool_free(c: &mut Cpu) {
    let p = super::mem().rd32(c.r[ESP] + 4);
    let base = game_mem().rd32(POOL_BASE);
    if base == 0 || !(base..base + POOL_BYTES).contains(&p) {
        // no pool (the exit) or not one of its nodes: as the original
        return call_original(c, POOL_FREE);
    }
    c.r[ESP] += 4;
    let mut g = POOL.lock().unwrap_or_else(|e| e.into_inner());
    if world::tick_running() {
        g.push(p);
    } else {
        pool_put(p);
    }
}

fn pool_put(p: u32) {
    let m = game_mem();
    m.wr32(p + 4, m.rd32(POOL_HEAD));
    m.wr32(POOL_HEAD, p);
    m.wr32(POOL_USED, m.rd32(POOL_USED).wrapping_sub(1));
}

/// The pool's nodes given back while the tick ran (at the join, after the merge).
fn pool_frees() {
    let list = std::mem::take(&mut *POOL.lock().unwrap_or_else(|e| e.into_inner()));
    let base = game_mem().rd32(POOL_BASE);
    for p in list.into_iter().filter(|p| base != 0 && (base..base + POOL_BYTES).contains(p)) {
        pool_put(p);
    }
}

// ---- the game's linked lists ------------------------------------------------------------------

/// llist methods (thiscall, the list in ecx: count +0, head +4, tail +8, cursor +0xc; nodes
/// from the pool: data, next +4, prev +8): remove the cursor's node, and three inserts. The
/// cursor is also moved inline everywhere (iterations, 0x504360 every frame).
pub const LLIST_OPS: [u32; 4] = [0x55e6d0, 0x55e740, 0x55e840, 0x55e910];
const LLIST_HEAD: usize = 16;

/// Lists changed (by an op) while a tick ran: list -> who (1 the main thread, 2 the tick).
/// A header is merged whole (`lists_merge`): by bytes, a cursor the tick moved over its own
/// list beat the drawing's emptying of that list (seen: a cursor on a node already back in the
/// pool, then the drawing's clearing loop 0x54ec00 walked the pool's free list for ever).
static LISTS: Mutex<std::collections::BTreeMap<u32, (u8, u32, u32)>> = Mutex::new(std::collections::BTreeMap::new());

fn llist_op(c: &mut Cpu, i: usize) {
    if inflight() {
        let who = if world::is_tick() { 2 } else { 1 };
        let ret = super::mem().rd32(c.r[ESP]);
        let mut l = LISTS.lock().unwrap_or_else(|e| e.into_inner());
        let e = l.entry(c.r[crate::rt::ECX]).or_insert((0, 0, 0));
        e.0 |= who;
        if who == 1 { e.1 = ret } else { e.2 = ret }
    }
    call_original(c, LLIST_OPS[i]);
}

/// Before the merge: the headers of lists only the main thread changed (as A has them). Lists
/// in `skip` (the tick thread's stack below the fork: its locals, the main thread's in A) are
/// left out, as `world::merge` leaves those bytes.
fn lists_before(skip: (u32, u32)) -> Vec<(u32, u8, [u8; LLIST_HEAD])> {
    let l = std::mem::take(&mut *LISTS.lock().unwrap_or_else(|e| e.into_inner()));
    let a = game_mem();
    let mut out = Vec::with_capacity(l.len());
    for (list, (who, main_ret, tick_ret)) in l {
        if (skip.0..skip.1).contains(&list) {
            continue;
        }
        if who == 3 {
            static SEEN: Mutex<std::collections::BTreeSet<u32>> = Mutex::new(std::collections::BTreeSet::new());
            if SEEN.lock().unwrap_or_else(|e| e.into_inner()).insert(list) {
                log!("tick: list {list:#x} changed by the drawing (from {main_ret:#x}) and the tick (from {tick_ret:#x}): the tick's kept");
            }
        }
        out.push((list, who, a.bytes(list, LLIST_HEAD).try_into().unwrap()));
    }
    out
}

/// After the merge: each changed list's header whole from the side that changed the list
/// (the tick's when both did).
fn lists_merge(lists: &[(u32, u8, [u8; LLIST_HEAD])]) {
    let (a, b) = (game_mem(), world::worlds().expect("worlds").b);
    for &(list, who, ref head) in lists {
        if who == 1 {
            a.slice(list, LLIST_HEAD).copy_from_slice(head);
        } else {
            a.slice(list, LLIST_HEAD).copy_from_slice(b.bytes(list, LLIST_HEAD));
        }
    }
}

// ---- the router's new objects ----------------------------------------------------------------

/// Router vt+0x50 (thiscall, no arguments): takes the objects queued on its list +0x906c (by
/// the router's add, vt+0x14 0x506830, from the tick) and sorts them into its containers
/// (vt+0x2c, +0x9088 ...), which the drawing walks. The world's drawing 0x4130a0 calls it at
/// its end (0x4139c8), after the tick in the original. While a tick runs, the tick queues on
/// the same list (seen: both changed it, then a call through a freed object); put off to
/// the join instead, the objects were missing from the pictures in between (all but the
/// player and the ground flickered). So the drawing waits for the tick here when A's queue
/// is not empty: the merged world, then the call, in the original's order. An empty queue
/// (the usual case: the picture after each join drains it) the drawing does not touch, so it
/// goes on without waiting.
pub const ROUTER_DRAIN: u32 = 0x503c80;
/// The router's queue list (`this` + this), its count at +0.
const ROUTER_QUEUE: u32 = 0x906c;
/// Joins at the router drain (its queue not empty), for the summary.
static DRAIN_JOINS: AtomicU32 = AtomicU32::new(0);

fn router_drain(c: &mut Cpu) {
    if !world::is_tick() && inflight() {
        let m = game_mem();
        let q = m.rd32(c.r[crate::rt::ECX] + ROUTER_QUEUE);
        if q == 0 || m.rd32(q) != 0 {
            DRAIN_JOINS.fetch_add(1, Ordering::Relaxed);
            join();
        }
    }
    call_original(c, ROUTER_DRAIN);
}

// ---- shared game state the main thread waits for --------------------------------------------

/// Functions that change state the tick changes too, with no merge rule yet: on the main thread
/// while a tick runs, they wait for it (join) first.
/// - sound_play 0x4641b0 (thiscall, ~30 callers): a voice for a sample, put on the sound
///   object's lists (+0x20 here, +0xd0 in sound_new_voice 0x465bc0). When both added to these
///   lists while a tick ran, one side's nodes were lost and a node later went back to the pool
///   twice (seen: the pool's free list empty, the game's assertion "headOfFreeElements", a fault).
/// A join is only safe where the main thread holds no pointers into the world: the light
/// manager's methods (0x490bf0 ...) joined in the middle of the drawing, which then called a
/// vertex buffer the tick had released.
pub const JOIN_FIRST: [u32; 1] = [0x4641b0];
/// Joins before each of `JOIN_FIRST`, for the summary.
static FIRST_JOINS: [AtomicU32; 1] = [const { AtomicU32::new(0) }; 1];

fn join_first(c: &mut Cpu, i: usize) {
    if !world::is_tick() && inflight() {
        if FIRST_JOINS[i].fetch_add(1, Ordering::Relaxed) == 0 {
            log!("tick: a join before {:#x}, called from {:#x}", JOIN_FIRST[i], game_mem().rd32(c.r[ESP]));
        }
        join();
    }
    call_original(c, JOIN_FIRST[i]);
}

// DEBUG lights
const DBG_LIGHT: [u32; 6] = [0x54f080, 0x54e860, 0x54ea60, 0x550a90, 0x54eb40, 0x490bf0];
static DBG: Mutex<std::collections::BTreeMap<(u32, u8, bool, u32), u32>> = Mutex::new(std::collections::BTreeMap::new());
fn dbg_light(c: &mut Cpu, i: usize) {
    let who = if world::is_tick() { 2 } else { 1 };
    let ret = super::mem().rd32(c.r[ESP]);
    *DBG.lock().unwrap().entry((DBG_LIGHT[i], who, inflight(), ret)).or_insert(0) += 1;
    call_original(c, DBG_LIGHT[i]);
}
fn dbg_dump() {
    let m = game_mem();
    let active: Vec<u32> = (0..128).filter(|&h| m.rd8(0x71e7ec + 0x80 + h) != 0).collect();
    let used = (0..128).filter(|&h| m.rd8(0x71e7ec + h) != 0).count();
    let d = std::mem::take(&mut *DBG.lock().unwrap());
    let v: Vec<String> = d.iter().map(|((f, w, i, r), n)| format!("{f:#x}/{w}/{}/{r:#x}*{n}", if *i { "fl" } else { "-" })).collect();
    log!("DEBUG lights: used {used} active {active:?}; calls {}", v.join(" "));
}

/// Hooks while the tick thread is on (`platform::hooks`).
pub const HOOKS: [(u32, crate::rt::Hook); 14] = [
    (DBG_LIGHT[0], |c| dbg_light(c, 0)),
    (DBG_LIGHT[1], |c| dbg_light(c, 1)),
    (DBG_LIGHT[2], |c| dbg_light(c, 2)),
    (DBG_LIGHT[3], |c| dbg_light(c, 3)),
    (DBG_LIGHT[4], |c| dbg_light(c, 4)),
    (DBG_LIGHT[5], |c| dbg_light(c, 5)),
    (ROUTER_DRAIN, router_drain),
    (JOIN_FIRST[0], |c| join_first(c, 0)),
    (POOL_ALLOC, pool_alloc),
    (POOL_FREE, pool_free),
    (LLIST_OPS[0], |c| llist_op(c, 0)),
    (LLIST_OPS[1], |c| llist_op(c, 1)),
    (LLIST_OPS[2], |c| llist_op(c, 2)),
    (LLIST_OPS[3], |c| llist_op(c, 3)),
];

/// Fork and join costs, logged every 250 ticks; in `check` mode the addresses both the tick
/// and the main thread changed.
fn note(synced: usize, fork: std::time::Duration, wait: std::time::Duration, join: std::time::Duration, m: world::Merged) {
    #[derive(Default)]
    struct Sum {
        n: u32,
        synced: usize,
        fork: f64,
        wait: f64,
        waits: u32,
        join: f64,
        copied: usize,
        merged: usize,
        conflicts: usize,
    }
    static S: Mutex<Option<Sum>> = Mutex::new(None);
    let mut g = S.lock().unwrap_or_else(|e| e.into_inner());
    let s = g.get_or_insert_with(Sum::default);
    s.n += 1;
    s.synced += synced;
    s.fork += fork.as_secs_f64() * 1e3;
    s.wait += wait.as_secs_f64() * 1e3;
    s.waits += (wait.as_micros() > 50) as u32;
    s.join += join.as_secs_f64() * 1e3;
    s.copied += m.copied;
    s.merged += m.merged;
    s.conflicts += m.conflicts;
    if world::mode(super::game_dir()) == world::Mode::Check {
        let mut at = CONFLICTS.lock().unwrap_or_else(|e| e.into_inner());
        for a in &m.at {
            *at.entry(*a).or_insert(0) += 1;
        }
    }
    if s.n % 50 == 0 {
        dbg_dump();
    }
    if s.n == 250 {
        let n = s.n as f64;
        log!(
            "tick: per tick fork {:.3} ms ({:.0} pages synced), waited {:.3} ms ({} of {} joins), join {:.3} ms ({:.0} pages copied, {:.0} merged, {:.1} bytes both changed), {} joined at the router drain, before {:?}",
            s.fork / n,
            s.synced as f64 / n,
            s.wait / n,
            s.waits,
            s.n,
            s.join / n,
            s.copied as f64 / n,
            s.merged as f64 / n,
            s.conflicts as f64 / n,
            DRAIN_JOINS.swap(0, Ordering::Relaxed),
            FIRST_JOINS.iter().map(|n| n.swap(0, Ordering::Relaxed)).collect::<Vec<_>>()
        );
        *g = None;
        let mut at = CONFLICTS.lock().unwrap_or_else(|e| e.into_inner());
        if !at.is_empty() {
            let mut v: Vec<(u32, u32)> = std::mem::take(&mut *at).into_iter().collect();
            v.sort_by_key(|e| std::cmp::Reverse(e.1));
            let list: Vec<String> = v.iter().take(40).map(|(a, n)| format!("{a:#x}*{n}")).collect();
            log!("tick: bytes both changed (address*ticks): {}", list.join(" "));
        }
    }
}

static CONFLICTS: Mutex<std::collections::BTreeMap<u32, u32>> = Mutex::new(std::collections::BTreeMap::new());
