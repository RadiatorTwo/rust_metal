//! `GUNMETAL_FRAMES=<file>`: the frame-time log, and finding aids for running the logic tick on
//! a second thread (docs/superpowers/plans/2026-10-30-tick-decoupling.md, steps 1 and (a)).
//!
//! Per picture one line in the file: when its Present was called (ms since the log started,
//! from QueryPerformanceCounter through `Instant`), the interval to the Present before, the
//! time to produce it (from game_frame's start, or from the last Present when it was drawn
//! outside game_frame), the host Present's own time, the logic ticks in its frame and their
//! time (first tick part's start to the last one's end, `fps::TICK_PARTS`), whether it was a
//! mission picture (variableUpdate on) and whether a page probe (below) ran in it. Every 2 s
//! and at the end (ExitProcess) a summary goes to the log: average, 99th percentile and
//! maximum of the intervals, for all pictures and separately for those whose frame ran a tick
//! and those without; fps. Pictures with a probe are left out of the summaries.
//!
//! Findings, logged once at the end:
//! 1. every import called while the main thread is inside a tick part, by name, with calls per
//!    tick (COM methods are thunks too, "IDirect3DDevice9::SetTexture"), what other guest
//!    threads call meanwhile, and the main thread's imports outside ticks per picture;
//! 2. time per tick part, and the tick's time outside its parts;
//! 3. guest pages written per tick and per picture, by region (the EXE's sections, the heap,
//!    Direct3D lock buffers, stacks), from the write watch the guest memory is reserved with
//!    (`rt::Memory::reserve`): every 20th picture its frame is split into "before the tick",
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

pub fn stats(v: &[f64]) -> Stats {
    if v.is_empty() {
        return Stats::default();
    }
    let mut s = v.to_vec();
    s.sort_by(f64::total_cmp);
    let rank = ((0.99 * s.len() as f64).ceil() as usize).clamp(1, s.len());
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
    #[cfg(windows)]
    watch: Vec<usize>,
}

static FRAMES: Mutex<Option<Frames>> = Mutex::new(None);

/// Turns the log on if `GUNMETAL_FRAMES` names a file; called on the main thread at start with
/// the EXE's sections (name, start, end).
pub fn init(sections: Vec<(String, u32, u32)>) {
    let Some(path) = std::env::var_os("GUNMETAL_FRAMES").filter(|p| !p.is_empty()) else { return };
    let mut out = std::io::BufWriter::new(std::fs::File::create(&path).expect("GUNMETAL_FRAMES file"));
    let _ = writeln!(out, "# present_ms interval_ms produce_ms present_call_ms ticks tick_ms mission probe");
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
    let pages = if r == 0 { s.watch[..count].iter().map(|&a| ((a - base as usize) >> 12) as u32).collect() } else { Vec::new() };
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

/// game_frame starts.
pub fn frame_start() {
    let m = mem();
    let p = m.rd32(super::fps::PROJ_DATA);
    let mission = p != 0 && m.rd32(p + 0x4c4) != 0;
    with(|s| {
        s.frame_start = Some(Instant::now());
        s.frame_mission = mission;
    });
}

/// game_frame ends.
pub fn frame_end() {
    with(|s| s.frame_start = None);
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

/// Tick part `i` (outermost) that started at `t` ended; the last part (0x413c60) ends a tick.
pub fn part_end(i: usize, t: Instant) {
    let now = Instant::now();
    with(|s| {
        let d = now - t;
        let p = &mut s.parts[i];
        p.0 += 1;
        p.1 += d;
        p.2 = p.2.max(d);
        if i == super::fps::TICK_PARTS.len() - 1 {
            if let Some(t0) = s.tick_start.take() {
                s.tick_time += now - t0;
                s.tick_wall += now - t0;
            }
            if s.probe.as_ref().is_some_and(|p| p.in_tick) {
                let pages = written(s);
                let p = s.probe.as_mut().unwrap();
                p.in_tick = false;
                union(&mut p.ticks, pages);
            }
        }
    });
}

/// An import (thunk `k`) is called on this thread; `in_tick`: the main thread is in a tick part.
pub fn import(k: usize, in_tick: bool) {
    let main = MAIN.get();
    with(|s| *s.imports.entry((in_tick, main, k)).or_default() += 1);
}

/// The game calls Present.
pub fn before_present() {
    with(|s| {
        let now = Instant::now();
        s.present_call = Some(now);
        if s.probe.as_ref().is_some_and(|p| !p.in_tick) {
            let pages = written(s);
            let p = s.probe.as_mut().unwrap();
            if p.ticked {
                union(&mut p.draw, pages);
            } else {
                union(&mut p.before, pages);
            }
        }
    });
}

/// The host's Present returned.
pub fn after_present() {
    let mut summary = None;
    with(|s| {
        let now = Instant::now();
        let call = s.present_call.take().unwrap_or(now);
        let interval = s.last_present.map_or(0.0, |l| (call - l).as_secs_f64() * 1e3);
        let produce = s.frame_start.or(s.last_present).map_or(0.0, |f| (call - f).as_secs_f64() * 1e3);
        let probe = s.probe.take();
        let probed = probe.is_some();
        if let Some(p) = probe {
            finish_probe(s, p, s.ticks);
        }
        let _ = writeln!(
            s.out,
            "{:.3} {interval:.3} {produce:.3} {:.3} {} {:.3} {} {}",
            (call - s.t0).as_secs_f64() * 1e3,
            (now - call).as_secs_f64() * 1e3,
            s.ticks,
            s.tick_time.as_secs_f64() * 1e3,
            s.frame_mission as u8,
            probed as u8
        );
        if s.last_present.is_some() {
            s.pictures.push(Picture { at: call, interval, ticks: s.ticks, mission: s.frame_mission, probe: probed });
        }
        s.last_present = Some(call);
        s.ticks = 0;
        s.tick_time = Duration::ZERO;
        // the next frame is probed: the write watch starts from here
        s.since_probe += 1;
        if s.since_probe >= PROBE_EVERY {
            s.since_probe = 0;
            s.probe = Some(Probe::default());
            let _ = written(s);
        }
        let start = s.pictures.get(s.window).map(|p| p.at);
        if let Some(start) = start.filter(|&t| call - t >= Duration::from_secs(2)) {
            let all = &s.pictures[s.window..];
            let w: Vec<(f64, u32)> = all.iter().filter(|p| !p.probe).map(|p| (p.interval, p.ticks)).collect();
            summary = Some(summary_of(&w, all.len(), (call - start).as_secs_f64()));
            s.window = s.pictures.len();
            let _ = s.out.flush();
        }
    });
    if let Some(sm) = summary {
        log!("frames: 2 s: {}", show_summary(&sm));
    }
}

/// The summary of the pictures without a probe in `w`; fps counts all `n` pictures.
fn summary_of(w: &[(f64, u32)], n: usize, seconds: f64) -> Summary {
    let mut sm = summary(w, seconds);
    sm.fps = if seconds > 0.0 { n as f64 / seconds } else { 0.0 };
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
    if let (Some(a), Some(b)) = (mission.first(), mission.last()) {
        let w: Vec<(f64, u32)> = mission.iter().filter(|p| !p.probe).map(|p| (p.interval, p.ticks)).collect();
        log!("frames: mission pictures: {}", show_summary(&summary_of(&w, mission.len(), (b.at - a.at).as_secs_f64())));
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
        .map(|&(lo, hi, k)| format!("{:#x}..{:#x} ({}, {k}/{})", lo << 12, (hi + 1) << 12, r.of(lo << 12), s.sum_both.n))
        .collect();
    log!("frames: (4) written by both, most often: {}", shown.join(", "));
}
