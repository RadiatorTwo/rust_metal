//! `GUNMETAL_FRAMES=<file>`: the frame-time log, and finding aids for running the logic tick on
//! a second thread (docs/superpowers/plans/2026-10-30-tick-decoupling.md, steps 1 and (a)).
//!
//! Per picture one line in the file: when its Present was called (ms since the log started,
//! from QueryPerformanceCounter through `Instant`), the interval to the Present before, the
//! time to produce it (from game_frame's start, or from the last Present when it was drawn
//! outside game_frame), the host Present's own time, the logic ticks in its frame and their
//! time (first tick part's start to the last one's end, `fps::TICK_PARTS`), whether it was a
//! mission picture (variableUpdate on) and whether a page probe (below) ran in it or in the
//! picture before (its finishing work lies in this interval); then where the main thread's time
//! went since the last Present returned (`COLUMNS`: imports, waits for the turn and which other
//! guest thread held it, the slowest import, page faults, the thread's CPU time). Every 2 s
//! and at the end (ExitProcess) a summary goes to the log: average, 99th percentile and
//! maximum of the intervals, for all pictures and separately for those whose frame ran a tick
//! and those without; fps. Pictures with a probe are left out of the summaries, and the first
//! mission picture (its interval reaches back into the loading) out of the mission's.
//!
//! Findings, logged once at the end:
//! 1. every import called while the main thread is inside a tick part, by name, with calls per
//!    tick (COM methods are thunks too, "IDirect3DDevice9::SetTexture"), what other guest
//!    threads call meanwhile, and the main thread's imports outside ticks per picture;
//! 2. time per tick part, and the tick's time outside its parts;
//! 3. guest pages written per tick and per picture, by region (the EXE's sections, the heap,
//!    Direct3D lock buffers, stacks), from the write watch the guest memory is reserved with
//!    (`rt::Memory::reserve`), only with `GUNMETAL_FRAMES_PAGES=1` (a probe costs ~4 ms of its
//!    picture and the next one's; without probes no picture is left out): every 20th picture its frame is split into "before the tick",
//!    "the ticks" and "drawing after the ticks up to Present", each with GetWriteWatch's reset;
//! 4. pages written both by the ticks and by the drawing after them, by region, and the most
//!    frequent ones. Reads are not observable this way; what the drawing reads is in the plan.
//!
//! Without `GUNMETAL_FRAMES` everything here is one relaxed load per call (`on`).

use super::{lock, log, mem};
use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

static ON: AtomicBool = AtomicBool::new(false);

/// The frame log is on (`GUNMETAL_FRAMES`).
#[inline(always)]
pub fn on() -> bool {
    ON.load(Ordering::Relaxed)
}

thread_local! {
    static MAIN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// this guest thread's start address (`thread_start`), for whose turn the main thread waited
    static LABEL: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    /// when this thread took the turn
    static TAKEN: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) };
}

/// Every how many pictures a page probe starts.
const PROBE_EVERY: u32 = 20;

// ---- summary maths ------------------------------------------------------------------------------

/// Average, 99th percentile (nearest rank) and maximum of some times (ms).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Stats {
    pub n: usize,
    pub avg: f64,
    pub p99: f64,
    pub max: f64,
}

/// The `p`-th percentile (nearest rank, 1..=100) of some times; 0 for none.
pub fn percentile(v: &[f64], p: usize) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    let mut s = v.to_vec();
    s.sort_by(f64::total_cmp);
    s[(s.len() * p.clamp(1, 100)).div_ceil(100).clamp(1, s.len()) - 1]
}

pub fn stats(v: &[f64]) -> Stats {
    if v.is_empty() {
        return Stats::default();
    }
    let mut s = v.to_vec();
    s.sort_by(f64::total_cmp);
    // nearest rank, in integers: ceil(0.99 n)
    let rank = (s.len() * 99).div_ceil(100).clamp(1, s.len());
    Stats { n: s.len(), avg: s.iter().sum::<f64>() / s.len() as f64, p99: s[rank - 1], max: s[s.len() - 1] }
}

/// The intervals between pictures, all of them and split by whether the picture's frame ran a
/// logic tick, and pictures per second over `seconds`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Summary {
    pub all: Stats,
    pub tick: Stats,
    pub no_tick: Stats,
    pub fps: f64,
}

/// From (interval ms, ticks in the picture's frame) of each picture.
pub fn summary(pictures: &[(f64, u32)], seconds: f64) -> Summary {
    let pick = |f: &dyn Fn(u32) -> bool| stats(&pictures.iter().filter(|p| f(p.1)).map(|p| p.0).collect::<Vec<_>>());
    Summary {
        all: pick(&|_| true),
        tick: pick(&|t| t > 0),
        no_tick: pick(&|t| t == 0),
        fps: if seconds > 0.0 { pictures.len() as f64 / seconds } else { 0.0 },
    }
}

fn show(s: &Stats) -> String {
    format!("{} x avg {:.2} p99 {:.2} max {:.2} ms", s.n, s.avg, s.p99, s.max)
}

fn show_summary(s: &Summary) -> String {
    format!("{:.1} fps; all {}; with a tick {}; without {}", s.fps, show(&s.all), show(&s.tick), show(&s.no_tick))
}

// ---- state --------------------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Picture {
    /// when its Present was called
    at: Instant,
    interval: f64,
    ticks: u32,
    mission: bool,
    probe: bool,
    /// the world update's (0x413c60) time in its frame, and the drawing's: from the last tick
    /// part's end (or game_frame's start) to Present
    p6: f64,
    draw: f64,
}

/// Pages written in parts of one probed frame (guest page numbers, ascending).
#[derive(Default)]
struct Probe {
    before: Vec<u32>,
    ticks: Vec<u32>,
    draw: Vec<u32>,
    /// the first tick started (its reset done)
    ticked: bool,
    /// a tick is running
    in_tick: bool,
    cost: Duration,
}

/// Sums of page counts over probes, total and by region.
#[derive(Default)]
struct PageSum {
    n: u32,
    pages: u64,
    max: usize,
    by_region: BTreeMap<String, u64>,
}

impl PageSum {
    fn add(&mut self, pages: &[u32], regions: &Regions) {
        self.n += 1;
        self.pages += pages.len() as u64;
        self.max = self.max.max(pages.len());
        for &p in pages {
            *self.by_region.entry(regions.of(p << 12)).or_default() += 1;
        }
    }
    fn line(&self, what: &str) -> String {
        if self.n == 0 {
            return format!("{what}: no probes");
        }
        let n = self.n as f64;
        let regions: Vec<String> = self.by_region.iter().map(|(r, k)| format!("{r} {:.1}", *k as f64 / n)).collect();
        format!("{what}: {} probes, avg {:.1} pages ({:.0} KB), max {}; by region: {}", self.n, self.pages as f64 / n, self.pages as f64 / n * 4.0, self.max, regions.join(", "))
    }
}

/// Named guest address ranges, for page findings.
struct Regions(Vec<(String, u32, u32)>);

impl Regions {
    fn of(&self, a: u32) -> String {
        self.0.iter().find(|r| (r.1..r.2).contains(&a)).map_or_else(|| "other".to_string(), |r| r.0.clone())
    }
}

struct Frames {
    out: std::io::BufWriter<std::fs::File>,
    t0: Instant,
    /// game_frame's start, if inside one
    frame_start: Option<Instant>,
    frame_mission: bool,
    last_present: Option<Instant>,
    /// this picture's ticks and their time
    ticks: u32,
    tick_time: Duration,
    tick_start: Option<Instant>,
    present_call: Option<Instant>,
    pictures: Vec<Picture>,
    /// the first picture of the current 2 s window
    window: usize,
    total_ticks: u64,
    /// per tick part: calls, time, longest
    parts: [(u64, Duration, Duration); 7],
    tick_wall: Duration,
    /// import calls (thunk index) by (inside a tick part, on the main thread)
    imports: HashMap<(bool, bool, usize), u64>,
    since_probe: u32,
    probe: Option<Probe>,
    /// page probes are on (`GUNMETAL_FRAMES_PAGES=1`)
    probes: bool,
    /// the last picture was probed: this one's interval holds the probe's finishing work
    after_probe: bool,
    /// the last picture was a mission picture
    last_mission: bool,
    /// the last tick part's end (a tick whose last part never ran ends there)
    last_part_end: Option<Instant>,
    /// where this picture's time went (the main thread), since the last Present returned
    detail: Detail,
    /// rdtsc and the clock at the start, for thread cycles in ms
    tsc0: (u64, Instant),
    faults0: u32,
    cycles0: u64,
    probe_cost: Vec<f64>,
    sections: Vec<(String, u32, u32)>,
    sum_before: PageSum,
    sum_ticks: PageSum,
    sum_draw: PageSum,
    sum_both: PageSum,
    sum_picture_tick: PageSum,
    sum_picture_no_tick: PageSum,
    /// ticks of the probed frames with ticks
    probed_ticks: u32,
    /// how often each page was written by both
    both: BTreeMap<u32, u32>,
    /// COM lock buffer pages written in ticks / by the drawing after them, by owner
    com_ticks: BTreeMap<String, u64>,
    com_draw: BTreeMap<String, u64>,
    /// this picture's world update time, and where its drawing started
    p6: Duration,
    drawn_from: Option<Instant>,
    #[cfg(windows)]
    watch: Vec<usize>,
}

static FRAMES: Mutex<Option<Frames>> = Mutex::new(None);

/// The file's columns: when Present was called, the interval to the Present before, the time
/// to produce the picture, the host Present, ticks and their time, mission picture, probed (or
/// right after a probe); then, from the last Present's return to this one's call (the main
/// thread): time in imports and their number, time waiting for the turn and how often, the
/// other guest threads' time with the turn (all, and the longest holder `start:ms`), the import
/// with the most time `name:ms`, our clear of the back buffer after the last Present, the
/// process's page faults, the main thread's CPU time (cycles, in ms of the TSC).
/// Last, the world update's (0x413c60) time in this frame and the drawing's (from the last tick
/// part's end, or game_frame's start, to Present).
const COLUMNS: &str = "present_ms interval_ms produce_ms present_call_ms ticks tick_ms mission probe imports_ms imports wait_ms waits others_ms holder top_import clear_ms faults cpu_ms p6_ms draw_ms";

#[derive(Default)]
struct Detail {
    imports: Duration,
    calls: u32,
    wait: Duration,
    waits: u32,
    /// other threads' time with the turn, by start address
    held: Vec<(u32, Duration)>,
    /// by thunk: time
    by_import: HashMap<usize, Duration>,
    clear: Duration,
}

#[cfg(target_arch = "x86_64")]
fn tsc() -> u64 {
    unsafe { core::arch::x86_64::_rdtsc() }
}
#[cfg(not(target_arch = "x86_64"))]
fn tsc() -> u64 {
    0
}

#[cfg(windows)]
fn page_faults() -> u32 {
    use windows_sys::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    let mut c: PROCESS_MEMORY_COUNTERS = unsafe { std::mem::zeroed() };
    c.cb = size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
    unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb) };
    c.PageFaultCount
}
#[cfg(not(windows))]
fn page_faults() -> u32 {
    0
}

/// The calling thread's CPU cycles (QueryThreadCycleTime, in TSC ticks).
#[cfg(windows)]
fn thread_cycles() -> u64 {
    use windows_sys::Win32::System::{Threading::GetCurrentThread, WindowsProgramming::QueryThreadCycleTime};
    let mut c = 0u64;
    unsafe { QueryThreadCycleTime(GetCurrentThread(), &mut c) };
    c
}
#[cfg(not(windows))]
fn thread_cycles() -> u64 {
    0
}

/// Turns the log on if `GUNMETAL_FRAMES` names a file; called on the main thread at start with
/// the EXE's sections (name, start, end).
pub fn init(sections: Vec<(String, u32, u32)>) {
    let Some(path) = std::env::var_os("GUNMETAL_FRAMES").filter(|p| !p.is_empty()) else { return };
    let mut out = std::io::BufWriter::new(std::fs::File::create(&path).expect("GUNMETAL_FRAMES file"));
    let _ = writeln!(out, "# {COLUMNS}");
    MAIN.set(true);
    *lock(&FRAMES) = Some(Frames {
        out,
        t0: Instant::now(),
        frame_start: None,
        frame_mission: false,
        last_present: None,
        ticks: 0,
        tick_time: Duration::ZERO,
        tick_start: None,
        present_call: None,
        pictures: Vec::new(),
        window: 0,
        total_ticks: 0,
        parts: Default::default(),
        tick_wall: Duration::ZERO,
        imports: HashMap::new(),
        since_probe: 0,
        probe: None,
        probes: std::env::var_os("GUNMETAL_FRAMES_PAGES").is_some_and(|v| v != "0"),
        after_probe: false,
        last_mission: false,
        last_part_end: None,
        detail: Detail::default(),
        tsc0: (tsc(), Instant::now()),
        faults0: page_faults(),
        cycles0: thread_cycles(),
        probe_cost: Vec::new(),
        sections,
        sum_before: PageSum::default(),
        sum_ticks: PageSum::default(),
        sum_draw: PageSum::default(),
        sum_both: PageSum::default(),
        sum_picture_tick: PageSum::default(),
        sum_picture_no_tick: PageSum::default(),
        probed_ticks: 0,
        both: BTreeMap::new(),
        com_ticks: BTreeMap::new(),
        com_draw: BTreeMap::new(),
        p6: Duration::ZERO,
        drawn_from: None,
        #[cfg(windows)]
        watch: vec![0; crate::rt::GUEST_SPAN / 4096 + 1],
    });
    ON.store(true, Ordering::Relaxed);
    log!("frames: logging pictures to {}", std::path::Path::new(&path).display());
}

fn with(f: impl FnOnce(&mut Frames)) {
    if let Some(s) = lock(&FRAMES).as_mut() {
        f(s)
    }
}

/// The guest pages written since the last call (write watch, reset), ascending.
#[cfg(windows)]
fn written(s: &mut Frames) -> Vec<u32> {
    use windows_sys::Win32::System::Memory::GetWriteWatch;
    let t = Instant::now();
    let mut count = s.watch.len();
    let mut gran = 0u32;
    let base = mem().base;
    let r = unsafe { GetWriteWatch(1, base as _, crate::rt::GUEST_SPAN, s.watch.as_mut_ptr() as _, &mut count, &mut gran) };
    let pages = if r == 0 {
        s.watch[..count].iter().map(|&a| ((a - base as usize) >> 12) as u32).collect()
    } else {
        static WARNED: AtomicBool = AtomicBool::new(false);
        if !WARNED.swap(true, Ordering::Relaxed) {
            log!("frames: warn: GetWriteWatch failed ({r:#x}): page findings are empty");
        }
        Vec::new()
    };
    if let Some(p) = s.probe.as_mut() {
        p.cost += t.elapsed();
    }
    pages
}

#[cfg(not(windows))]
fn written(_: &mut Frames) -> Vec<u32> {
    Vec::new()
}

/// Pages in both sorted lists.
fn common(a: &[u32], b: &[u32]) -> Vec<u32> {
    let (mut i, mut j, mut out) = (0, 0, Vec::new());
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                out.push(a[i]);
                i += 1;
                j += 1;
            }
        }
    }
    out
}

fn union(a: &mut Vec<u32>, b: Vec<u32>) {
    a.extend(b);
    a.sort_unstable();
    a.dedup();
}

/// The regions now: the EXE's sections, stacks, lock buffers, the heap.
fn regions(s: &Frames) -> Regions {
    let mut r = vec![("main stack".to_string(), super::STACK_LO, super::STACK_HI)];
    r.extend(super::crash::thread_stacks().into_iter().map(|(lo, hi)| ("thread stacks".to_string(), lo, hi)));
    r.extend(super::com::shadow_ranges().into_iter().map(|(lo, hi)| ("COM lock buffers (D3D, DirectSound)".to_string(), lo, hi)));
    r.extend(s.sections.iter().map(|(n, lo, hi)| (format!("EXE {n}"), *lo, *hi)));
    r.push(("heap".to_string(), super::ARENA_LO, super::ARENA_HI));
    Regions(r)
}

// ---- events -------------------------------------------------------------------------------------

/// The calling host thread runs a guest thread started at `start` (for the turn's holders).
pub fn thread_start(start: u32) {
    LABEL.set(start);
}

/// The calling thread got the turn after waiting `waited` (None: it did not have to wait).
pub fn turn_taken(waited: Option<Duration>) {
    TAKEN.set(Some(Instant::now()));
    if let Some(w) = waited.filter(|_| MAIN.get()) {
        with(|s| {
            s.detail.wait += w;
            s.detail.waits += 1;
        });
    }
}

/// The calling thread gives up the turn.
pub fn turn_given() {
    let Some(t) = TAKEN.take() else { return };
    if MAIN.get() {
        return;
    }
    let d = t.elapsed();
    let label = LABEL.get();
    with(|s| match s.detail.held.iter_mut().find(|h| h.0 == label) {
        Some(h) => h.1 += d,
        None => s.detail.held.push((label, d)),
    });
}

/// game_frame starts.
pub fn frame_start() {
    let m = mem();
    let p = m.rd32(super::fps::PROJ_DATA);
    let mission = p != 0 && m.rd32(p + 0x4c4) != 0;
    with(|s| {
        let now = Instant::now();
        s.frame_start = Some(now);
        s.drawn_from = Some(now);
        s.frame_mission = mission;
    });
}

/// game_frame ends.
pub fn frame_end() {
    with(|s| {
        s.frame_start = None;
        end_tick(s);
    });
}

/// A tick part starts (outermost); the first one after a tick's end starts a tick. Returns
/// the part's start.
pub fn part_start() -> Instant {
    let now = Instant::now();
    with(|s| {
        if s.tick_start.is_none() {
            s.tick_start = Some(now);
            s.ticks += 1;
            s.total_ticks += 1;
            if s.probe.is_some() {
                let pages = written(s);
                let p = s.probe.as_mut().unwrap();
                p.ticked = true;
                p.in_tick = true;
                union(&mut p.before, pages);
            }
        }
    });
    now
}

/// The running tick ends at its last part's end: when the last part (0x413c60) returns, or, if
/// a tick stopped before it, when game_frame returns or Present is called.
fn end_tick(s: &mut Frames) {
    let Some(t0) = s.tick_start.take() else { return };
    let end = s.last_part_end.unwrap_or(t0).max(t0);
    s.tick_time += end - t0;
    s.tick_wall += end - t0;
    if s.probe.as_ref().is_some_and(|p| p.in_tick) {
        let pages = written(s);
        let p = s.probe.as_mut().unwrap();
        p.in_tick = false;
        union(&mut p.ticks, pages);
    }
}

/// Tick part `i` (outermost) that started at `t` ended; the last part (0x413c60) ends a tick.
pub fn part_end(i: usize, t: Instant) {
    let now = Instant::now();
    with(|s| {
        let d = now - t;
        let p = &mut s.parts[i];
        p.0 += 1;
        p.1 += d;
        p.2 = p.2.max(d);
        s.last_part_end = Some(now);
        s.drawn_from = Some(now);
        if i == super::fps::TICK_PARTS.len() - 1 {
            s.p6 += d;
            end_tick(s);
        }
    });
}

/// An import (thunk `k`) is called on this thread; `in_tick`: the main thread is in a tick part.
pub fn import(k: usize, in_tick: bool) {
    let main = MAIN.get();
    with(|s| *s.imports.entry((in_tick, main, k)).or_default() += 1);
}

/// Import `k` returned after `d` (its own time, without the wait for the turn after it).
pub fn import_done(k: usize, d: Duration) {
    if !MAIN.get() {
        return;
    }
    with(|s| {
        s.detail.imports += d;
        s.detail.calls += 1;
        *s.detail.by_import.entry(k).or_default() += d;
    });
}

/// Our clear of the back buffer after Present took `d`.
pub fn cleared(d: Duration) {
    with(|s| s.detail.clear += d);
}

/// The game calls Present.
pub fn before_present() {
    with(|s| {
        end_tick(s);
        if s.probe.as_ref().is_some_and(|p| !p.in_tick) {
            let pages = written(s);
            let p = s.probe.as_mut().unwrap();
            if p.ticked {
                union(&mut p.draw, pages);
            } else {
                union(&mut p.before, pages);
            }
        }
        // after the probe's work: it belongs to this (probed) picture
        s.present_call = Some(Instant::now());
    });
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

/// The host's Present returned.
pub fn after_present() {
    let mut summary = None;
    let faults = page_faults();
    let cycles = thread_cycles();
    with(|s| {
        let now = Instant::now();
        let call = s.present_call.take().unwrap_or(now);
        let interval = s.last_present.map_or(0.0, |l| ms(call - l));
        let produce = s.frame_start.or(s.last_present).map_or(0.0, |f| ms(call - f));
        let p6 = ms(std::mem::take(&mut s.p6));
        let draw = s.drawn_from.take().or(s.last_present).map_or(0.0, |f| ms(call - f));
        let probe = s.probe.take();
        let probed = probe.is_some();
        if let Some(p) = probe {
            finish_probe(s, p, s.ticks);
        }
        // a probe's finishing work (above) lies in the next picture's interval
        let disturbed = probed || s.after_probe;
        s.after_probe = probed;
        let d = std::mem::take(&mut s.detail);
        let others: Duration = d.held.iter().map(|h| h.1).sum();
        let holder = d.held.iter().max_by_key(|h| h.1).map_or("-".to_string(), |h| format!("{:#x}:{:.2}", h.0, ms(h.1)));
        let top = d
            .by_import
            .iter()
            .max_by_key(|e| *e.1)
            .map_or("-".to_string(), |(&k, &t)| format!("{}:{:.2}", super::thunk_name(k).replace(' ', "_"), ms(t)));
        let tsc_per_ms = tsc().wrapping_sub(s.tsc0.0) as f64 / ms(s.tsc0.1.elapsed()).max(1e-9);
        let _ = writeln!(
            s.out,
            "{:.3} {interval:.3} {produce:.3} {:.3} {} {:.3} {} {} {:.3} {} {:.3} {} {:.3} {holder} {top} {:.3} {} {:.3} {p6:.3} {draw:.3}",
            ms(call - s.t0),
            ms(now - call),
            s.ticks,
            ms(s.tick_time),
            s.frame_mission as u8,
            disturbed as u8,
            ms(d.imports),
            d.calls,
            ms(d.wait),
            d.waits,
            ms(others),
            ms(d.clear),
            faults.wrapping_sub(s.faults0),
            cycles.wrapping_sub(s.cycles0) as f64 / tsc_per_ms,
        );
        s.faults0 = faults;
        s.cycles0 = cycles;
        if s.last_present.is_some() {
            // the first mission picture's interval reaches back into the loading: not a mission one
            let mission = s.frame_mission && s.last_mission;
            s.pictures.push(Picture { at: call, interval, ticks: s.ticks, mission, probe: disturbed, p6, draw });
        }
        s.last_mission = s.frame_mission;
        s.last_present = Some(call);
        s.ticks = 0;
        s.tick_time = Duration::ZERO;
        // the next frame is probed: the write watch starts from here
        s.since_probe += 1;
        if s.probes && s.since_probe >= PROBE_EVERY {
            s.since_probe = 0;
            s.probe = Some(Probe::default());
            let _ = written(s);
        }
        let start = s.pictures.get(s.window).map(|p| p.at);
        if start.is_some_and(|t| call - t >= Duration::from_secs(2)) {
            summary = Some(window_summary(&s.pictures[s.window..]));
            s.window = s.pictures.len();
            let _ = s.out.flush();
        }
    });
    if let Some(sm) = summary {
        log!("frames: 2 s: {}", show_summary(&sm));
    }
}

/// The summary of some pictures: the intervals of those without a probe; fps from all of them,
/// their number over the sum of their intervals.
fn window_summary<'a>(all: impl IntoIterator<Item = &'a Picture> + Clone) -> Summary {
    let w: Vec<(f64, u32)> = all.clone().into_iter().filter(|p| !p.probe).map(|p| (p.interval, p.ticks)).collect();
    let (n, ms) = all.into_iter().fold((0usize, 0.0), |a, p| (a.0 + 1, a.1 + p.interval));
    let mut sm = summary(&w, 1.0);
    sm.fps = if ms > 0.0 { n as f64 * 1e3 / ms } else { 0.0 };
    sm
}

fn finish_probe(s: &mut Frames, p: Probe, ticks: u32) {
    s.probe_cost.push(p.cost.as_secs_f64() * 1e3);
    if !p.ticked {
        // a frame without a tick: all of it is drawing
        let r = regions(s);
        s.sum_picture_no_tick.add(&p.before, &r);
        return;
    }
    if p.in_tick {
        return;
    }
    let r = regions(s);
    let owners = super::com::shadow_owners();
    let owner = |pg: u32| {
        let (lo, hi) = (pg << 12, (pg << 12) + 0x1000);
        // a shadow covering the whole page, or sharing it with other heap blocks
        owners.iter().find(|o| o.0 < hi && lo < o.1).map(|o| format!("{} ({})", o.2, if o.0 <= lo && hi <= o.1 { "whole page" } else { "shares the page" }))
    };
    for (pages, sum) in [(&p.ticks, &mut s.com_ticks), (&p.draw, &mut s.com_draw)] {
        for &pg in pages.iter() {
            if let Some(o) = owner(pg) {
                *sum.entry(o).or_default() += 1;
            }
        }
    }
    let both = common(&p.ticks, &p.draw);
    for &pg in &both {
        *s.both.entry(pg).or_default() += 1;
    }
    let mut all = p.before.clone();
    union(&mut all, p.ticks.clone());
    union(&mut all, p.draw.clone());
    s.sum_before.add(&p.before, &r);
    s.sum_ticks.add(&p.ticks, &r);
    s.sum_draw.add(&p.draw, &r);
    s.sum_both.add(&both, &r);
    s.sum_picture_tick.add(&all, &r);
    s.probed_ticks += ticks;
}

/// The end of the run (ExitProcess): the summary of the mission's pictures and the findings.
pub fn finish() {
    if !on() {
        return;
    }
    let Some(mut s) = lock(&FRAMES).take() else { return };
    ON.store(false, Ordering::Relaxed);
    let _ = s.out.flush();
    let mission: Vec<&Picture> = s.pictures.iter().filter(|p| p.mission).collect();
    if !mission.is_empty() {
        log!("frames: mission pictures: {}", show_summary(&window_summary(mission.iter().copied())));
        // the world update against the drawing, per picture (step 6 of the tick thread's plan)
        let clean: Vec<&&Picture> = mission.iter().filter(|p| !p.probe).collect();
        let dist = |v: &[f64]| {
            let st = stats(v);
            format!("{} x avg {:.2} p50 {:.2} p99 {:.2} max {:.2} ms", st.n, st.avg, percentile(v, 50), st.p99, st.max)
        };
        let ticked: Vec<&&&Picture> = clean.iter().filter(|p| p.ticks > 0).collect();
        let p6: Vec<f64> = ticked.iter().map(|p| p.p6).collect();
        let draw_t: Vec<f64> = ticked.iter().map(|p| p.draw).collect();
        let draw_n: Vec<f64> = clean.iter().filter(|p| p.ticks == 0).map(|p| p.draw).collect();
        let longer = ticked.iter().filter(|p| p.p6 > p.draw).count();
        let ratio: Vec<f64> = ticked.iter().filter(|p| p.draw > 0.0).map(|p| p.p6 / p.draw).collect();
        log!("frames: (5) world update 0x413c60 per picture with a tick: {}", dist(&p6));
        log!("frames: (5) drawing (last tick part's end to Present) with a tick: {}; without a tick: {}", dist(&draw_t), dist(&draw_n));
        log!(
            "frames: (5) the world update took longer than the drawing after it in {longer} of {} pictures with a tick ({:.1}%); p6/draw: {}",
            ticked.len(),
            100.0 * longer as f64 / ticked.len().max(1) as f64,
            dist(&ratio).replace(" ms", "")
        );
    }
    let names = |k: usize| super::thunk_name(k);
    let ticks = s.total_ticks.max(1) as f64;
    let pictures = s.pictures.len().max(1) as f64;
    let list = |in_tick: bool, main: bool, per: f64| {
        let mut v: Vec<(String, u64)> = s.imports.iter().filter(|e| e.0.0 == in_tick && e.0.1 == main).map(|e| (names(e.0.2), *e.1)).collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        v.iter().map(|(n, k)| format!("{n} {k} ({:.2})", *k as f64 / per)).collect::<Vec<_>>().join(", ")
    };
    log!("frames: findings over {} ticks and {} pictures", s.total_ticks, s.pictures.len());
    log!("frames: (1) imports of the main thread inside tick parts (calls, per tick): {}", list(true, true, ticks));
    log!("frames: (1) imports of other threads while the main thread is in a tick part (calls, per tick): {}", list(true, false, ticks));
    log!("frames: (1) imports of the main thread outside tick parts (calls, per picture): {}", list(false, true, pictures));
    let parts_sum: Duration = s.parts.iter().map(|p| p.1).sum();
    for (i, p) in s.parts.iter().enumerate() {
        let a = super::fps::TICK_PARTS[i];
        let avg = if p.0 > 0 { p.1.as_secs_f64() * 1e3 / p.0 as f64 } else { 0.0 };
        log!("frames: (2) tick part {}: {} calls, avg {avg:.3} ms, max {:.3} ms, {:.1}% of the parts", crate::image::named(a), p.0, p.2.as_secs_f64() * 1e3, 100.0 * p.1.as_secs_f64() / parts_sum.as_secs_f64().max(1e-9));
    }
    log!(
        "frames: (2) per tick: {:.3} ms from the first part's start to the last one's end, {:.3} ms in the parts",
        s.tick_wall.as_secs_f64() * 1e3 / ticks,
        parts_sum.as_secs_f64() * 1e3 / ticks
    );
    let probes = stats(&s.probe_cost);
    log!("frames: (3) write-watch probes: {} (cost per probed picture: avg {:.2} ms, max {:.2} ms); probed frames with ticks ran {} ticks", probes.n, probes.avg, probes.max, s.probed_ticks);
    log!("frames: (3) {}", s.sum_ticks.line("pages written in a frame's ticks"));
    log!("frames: (3) {}", s.sum_draw.line("pages written after the ticks up to Present (drawing)"));
    log!("frames: (3) {}", s.sum_before.line("pages written from the last Present to the first tick"));
    log!("frames: (3) {}", s.sum_picture_tick.line("pages written per picture, frames with ticks"));
    log!("frames: (3) {}", s.sum_picture_no_tick.line("pages written per picture, frames without a tick"));
    log!("frames: (4) {}", s.sum_both.line("pages written both by the ticks and by the drawing after them"));
    // the pages most often written by both, as runs of neighbouring pages
    let r = regions(&s);
    let mut runs: Vec<(u32, u32, u32)> = Vec::new();
    for (&pg, &k) in &s.both {
        match runs.last_mut() {
            Some(run) if run.1 + 1 == pg => {
                run.1 = pg;
                run.2 = run.2.max(k);
            }
            _ => runs.push((pg, pg, k)),
        }
    }
    runs.sort_by(|a, b| b.2.cmp(&a.2).then((b.1 - b.0).cmp(&(a.1 - a.0))));
    let shown: Vec<String> = runs
        .iter()
        .take(24)
        .map(|&(lo, hi, k)| format!("{:#x}..{:#x} ({}, {k}/{})", lo << 12, (u64::from(hi) + 1) << 12, r.of(lo << 12), s.sum_both.n))
        .collect();
    log!("frames: (4) written by both, most often: {}", shown.join(", "));
    for (what, sum) in [("in ticks", &s.com_ticks), ("by the drawing after the ticks", &s.com_draw)] {
        let n = s.sum_ticks.n.max(1) as f64;
        let mut v: Vec<(&String, &u64)> = sum.iter().collect();
        v.sort_by(|a, b| b.1.cmp(a.1));
        let l: Vec<String> = v.iter().take(20).map(|(o, k)| format!("{o}: {:.1}", **k as f64 / n)).collect();
        log!("frames: (3) COM lock buffer pages written {what}, per probed frame, by owner: {}", l.join("; "));
    }
}
