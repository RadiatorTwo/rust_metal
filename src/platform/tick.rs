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
//! overlap); `1` and `check` do the same for now (overlap is step 4).
//!
//! When: in a mission (variableUpdate on) after `STEADY` frames in a row, with the loader
//! thread (0x4eadf0) ended and the device not lost; otherwise P6 runs on the main thread as
//! before.

use super::{STACK_HI, STACK_LO, crash, free, game_mem, log, replay_calls, world};
use crate::rt::{Cpu, ESP, call_original};
use std::sync::atomic::{AtomicU32, Ordering};
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

/// P6 called by game_frame's tick loop (its return address and argument on the stack, this
/// in ecx): run it on the tick thread. False when not now: the caller runs it as usual.
pub fn world_update(c: &mut Cpu) -> bool {
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
    let synced = if first { 0 } else { world::sync(w, &ranges) };
    let esp = c.r[ESP];
    world::copy_range(w.a, &[w.b, w.c], esp & !0xfff, STACK_HI);
    world::FREES.set_running(true);
    let mut cpu = Box::new(c.clone());
    cpu.mem = w.b.base;
    let job = Job { cpu, tid: super::kernel32::thread_id(), tls: super::kernel32::tls_values(), last_error: super::LAST_ERROR.get() };
    let wk = worker();
    let forked = t0.elapsed();
    wk.jobs.send(job).expect("tick thread");
    // serial: the main thread waits at once
    let done = super::without_turn_result(|| wk.done.lock().unwrap_or_else(|e| e.into_inner()).recv().expect("tick thread"));
    let t1 = std::time::Instant::now();
    // join: the tick's pages into A, but not its stack below the fork (the main thread's)
    let merged = world::merge(w, &ranges, (STACK_LO, esp));
    world::FREES.set_running(false);
    for p in world::FREES.take() {
        free(p);
    }
    replay_calls(&world::take_calls());
    // the registers as P6 left them (ret 4 popped), memory A
    let mem = c.mem;
    *c = (*done).clone();
    c.mem = mem;
    if first {
        log!("tick: first fork at {} ms", super::kernel32::ms_since_start());
    }
    note(synced, forked, t1.elapsed(), merged);
    true
}

/// Fork and join costs, logged every 250 ticks.
fn note(synced: usize, fork: std::time::Duration, join: std::time::Duration, m: world::Merged) {
    #[derive(Default)]
    struct Sum {
        n: u32,
        synced: usize,
        fork: f64,
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
    s.join += join.as_secs_f64() * 1e3;
    s.copied += m.copied;
    s.merged += m.merged;
    s.conflicts += m.conflicts;
    if s.n == 250 {
        let n = s.n as f64;
        log!(
            "tick: per tick fork {:.3} ms ({:.0} pages synced), join {:.3} ms ({:.0} pages copied, {:.0} merged, {:.1} bytes both changed)",
            s.fork / n,
            s.synced as f64 / n,
            s.join / n,
            s.copied as f64 / n,
            s.merged as f64 / n,
            s.conflicts as f64 / n
        );
        *g = None;
    }
}
