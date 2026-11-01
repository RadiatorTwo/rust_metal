//! Three copies of guest memory for the tick on a second thread (plan
//! docs/superpowers/plans/2026-10-31-tick-thread.md): A is the game's memory (`mem()` on every
//! thread but the tick's), B the one the world update runs in on the tick thread, C the base:
//! A as it was when the tick started, so that a page both sides wrote can be merged by bytes.
//!
//! What changed is found by each memory's page map (`rt::MAP_OFF`): generated stores mark
//! their page when `rt::marking()` (the tick thread is on), host writes through `Memory::wr*`,
//! `slice` and `mark`. Write watch was the plan's first choice, but a GetWriteWatch call with
//! reset costs ~0.6 ms whatever the range. A marked page stands for itself and the first
//! `SPILL` bytes of the next page (a store across a page end marks only its first byte), and
//! stores through esp are not marked: stacks are copied whole (`copy_range`).
//!
//! Invariant between ticks: B and C equal A except on the pages marked in A's map (and the
//! stacks). `sync` copies those; `merge` brings the tick's pages back into A and marks them
//! in A, so the next `sync` makes B and C equal again.

use crate::rt::{Memory, PAGES};
use std::path::Path;
use std::sync::OnceLock;
use std::sync::atomic::Ordering;

/// Bytes of the next page a marked page stands for: the widest store generated code makes
/// (8: a double, a qword) minus 1, rounded up.
pub const SPILL: usize = 8;
const PAGE: usize = 4096;

/// The switch: registry `TickThread` (DWORD in the game's key) and `GUNMETAL_TICK_THREAD`
/// (`0` off, `1` on, `serial`: fork and wait at once, `check`: with conflict reports).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Off,
    On,
    Serial,
    Check,
}

/// The tick thread's mode, read once; `dir` is the game folder (its registry.txt).
pub fn mode(dir: &Path) -> Mode {
    static MODE: OnceLock<Mode> = OnceLock::new();
    *MODE.get_or_init(|| {
        let env = std::env::var("GUNMETAL_TICK_THREAD").ok();
        let v = env.or_else(|| {
            super::set_game_dir(dir);
            let (_, d) = super::advapi32::value("", "TickThread")?;
            Some(u32::from_le_bytes(d.get(..4)?.try_into().ok()?).to_string())
        });
        match v.as_deref() {
            Some("1") => Mode::On,
            Some("serial") => Mode::Serial,
            Some("check") => Mode::Check,
            _ => Mode::Off,
        }
    })
}

// ---- thread roles --------------------------------------------------------------------------

/// What a host thread is to the worlds: the game's main thread, the tick thread (in B, posing
/// as the main thread), or any other (guest threads, host workers).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    Other,
    Main,
    Tick,
}

thread_local! {
    static ROLE: std::cell::Cell<Role> = const { std::cell::Cell::new(Role::Other) };
}

pub fn role() -> Role {
    ROLE.with(|r| r.get())
}

pub fn set_role(r: Role) {
    ROLE.with(|c| c.set(r));
}

/// On the tick thread: it takes no turn, imports follow `tick_rule`.
pub fn is_tick() -> bool {
    role() == Role::Tick
}

// ---- frees while a tick runs -------------------------------------------------------------------

/// Frees held back while a tick runs: the block may still be in use in the other world, and
/// the heap (one for all worlds) must not hand it out again before the join.
pub struct FreeQueue {
    running: std::sync::atomic::AtomicBool,
    list: std::sync::Mutex<Vec<u32>>,
}

impl FreeQueue {
    pub const fn new() -> FreeQueue {
        FreeQueue { running: std::sync::atomic::AtomicBool::new(false), list: std::sync::Mutex::new(Vec::new()) }
    }
    /// Whether a tick runs in B (from its fork to its join).
    pub fn running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }
    pub fn set_running(&self, on: bool) {
        self.running.store(on, Ordering::Release);
    }
    /// Queue `p` while a tick runs (true), else leave it to the caller (false).
    pub fn defer(&self, p: u32) -> bool {
        if !self.running() {
            return false;
        }
        self.list.lock().unwrap_or_else(|e| e.into_inner()).push(p);
        true
    }
    /// The frees queued while the tick ran, to be done now.
    pub fn take(&self) -> Vec<u32> {
        std::mem::take(&mut *self.list.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

impl Default for FreeQueue {
    fn default() -> Self {
        Self::new()
    }
}

/// The heap's queue (`platform::free`).
pub static FREES: FreeQueue = FreeQueue::new();

pub fn tick_running() -> bool {
    FREES.running()
}

pub fn defer_free(p: u32) -> bool {
    FREES.defer(p)
}

// ---- imports on the tick thread ----------------------------------------------------------------

/// What an import called on the tick thread does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TickRule {
    /// called as on any thread (memory, critical sections, time, sound, input)
    Live,
    /// a Direct3D setter: recorded with its arguments (and the bytes argument `.0` points at,
    /// `.1` of them, when `.1` is not 0), D3D_OK returned; replayed on the main thread at the
    /// join (`take_calls`)
    Record(usize, usize),
    /// not expected in a tick: called live, logged once
    Unexpected,
}

/// The rule for an import of this name (`Dll!Name` or `Interface::Method` as thunks are named).
pub fn tick_rule(name: &str) -> TickRule {
    if let Some(m) = name.strip_prefix("IDirect3DDevice9::") {
        return match m {
            "SetLight" | "SetTransform" => TickRule::Record(2, if m == "SetLight" { 104 } else { 64 }),
            "SetMaterial" => TickRule::Record(1, 68),
            "LightEnable" | "SetRenderState" => TickRule::Record(0, 0),
            _ => TickRule::Unexpected,
        };
    }
    if name.starts_with("IDirectSound") || name.starts_with("IDirectInput") {
        return TickRule::Live;
    }
    const LIVE: &[&str] = &[
        "HeapAlloc", "HeapFree", "HeapReAlloc", "HeapSize", "GlobalAlloc", "GlobalFree", "VirtualAlloc", "VirtualFree", "EnterCriticalSection",
        "LeaveCriticalSection", "InitializeCriticalSection", "DeleteCriticalSection", "TlsGetValue", "GetLastError", "SetLastError",
        "InterlockedIncrement", "InterlockedDecrement", "InterlockedExchange", "InterlockedCompareExchange", "GetCurrentThreadId",
        "GetCurrentThread", "timeGetTime", "GetTickCount", "QueryPerformanceCounter", "QueryPerformanceFrequency", "OutputDebugStringA",
    ];
    if LIVE.contains(&name) { TickRule::Live } else { TickRule::Unexpected }
}

/// A Direct3D call recorded on the tick thread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Call {
    /// the thunk
    pub k: usize,
    /// its stack arguments (`this` first)
    pub args: Vec<u32>,
    /// the argument that points at data, and a copy of the data
    pub data: Option<(usize, Vec<u8>)>,
}

static CALLS: std::sync::Mutex<Vec<Call>> = std::sync::Mutex::new(Vec::new());

/// Record an import call of thunk `k` with `nargs` stack arguments by `rule` (a `Record`).
pub fn record(k: usize, args: Vec<u32>, rule: TickRule, mem: &Memory) {
    let TickRule::Record(at, n) = rule else { return };
    let data = (n != 0 && args.get(at).is_some_and(|&p| p != 0)).then(|| (at, mem.bytes(args[at], n).to_vec()));
    CALLS.lock().unwrap_or_else(|e| e.into_inner()).push(Call { k, args, data });
}

/// The calls recorded in the tick, in order (for the replay at the join).
pub fn take_calls() -> Vec<Call> {
    std::mem::take(&mut *CALLS.lock().unwrap_or_else(|e| e.into_inner()))
}

/// The three memories.
pub struct Worlds {
    pub a: &'static Memory,
    pub b: &'static Memory,
    pub c: &'static Memory,
}

static WORLDS: OnceLock<Worlds> = OnceLock::new();

/// Reserve B and C next to the game's memory `a` (once): from now on every `map` of A maps
/// them too, and what A has mapped is mapped in them now. Nothing is copied yet (`initial`).
pub fn setup(a: &'static Memory) -> &'static Worlds {
    WORLDS.get_or_init(|| {
        let b: &'static Memory = Box::leak(Box::new(Memory::reserve_with(false)));
        let c: &'static Memory = Box::leak(Box::new(Memory::reserve_with(false)));
        a.set_twins(vec![b, c]);
        Worlds { a, b, c }
    })
}

pub fn worlds() -> Option<&'static Worlds> {
    WORLDS.get()
}

/// Mapped page ranges [lo, hi) in page numbers, sorted and joined.
pub fn page_ranges(m: &Memory) -> Vec<(usize, usize)> {
    let mut r: Vec<(usize, usize)> = m.ranges().iter().map(|&(lo, hi)| (lo as usize / PAGE, (hi as usize).div_ceil(PAGE))).collect();
    r.sort_unstable();
    let mut out: Vec<(usize, usize)> = Vec::with_capacity(r.len());
    for (lo, hi) in r {
        match out.last_mut() {
            Some(l) if lo <= l.1 => l.1 = l.1.max(hi),
            _ => out.push((lo, hi)),
        }
    }
    out
}

/// The marked pages of `m` inside `ranges`, in order; with `clear` their marks are taken
/// (a write after that marks the page again).
pub fn marked(m: &Memory, ranges: &[(usize, usize)], clear: bool, out: &mut Vec<usize>) {
    let map = m.pages();
    for &(lo, hi) in ranges {
        for (p, b) in map[lo..hi.min(PAGES)].iter().enumerate() {
            if b.load(Ordering::Relaxed) != 0 {
                if clear {
                    b.swap(0, Ordering::AcqRel);
                }
                out.push(lo + p);
            }
        }
    }
}

fn mapped(ranges: &[(usize, usize)], p: usize) -> bool {
    ranges.iter().any(|&(lo, hi)| lo <= p && p < hi)
}

/// The bytes page `p` stands for: the page and, when the next page is mapped, its first
/// `SPILL` bytes.
fn extent(ranges: &[(usize, usize)], p: usize) -> usize {
    if mapped(ranges, p + 1) { PAGE + SPILL } else { PAGE }
}

unsafe fn at(m: &Memory, p: usize) -> *mut u8 {
    unsafe { m.base.add(p * PAGE) }
}

/// Copy pages (with their spill) from `from` to each of `to`.
pub fn copy_pages(from: &Memory, to: &[&Memory], ranges: &[(usize, usize)], pages: &[usize]) {
    for &p in pages {
        let n = extent(ranges, p);
        for t in to {
            unsafe { std::ptr::copy_nonoverlapping(at(from, p), at(t, p), n) };
        }
    }
}

/// Copy guest bytes [lo, hi) from `from` to each of `to` (stacks, whole ranges).
pub fn copy_range(from: &Memory, to: &[&Memory], lo: u32, hi: u32) {
    for t in to {
        unsafe { std::ptr::copy_nonoverlapping(from.base.add(lo as usize), t.base.add(lo as usize), (hi - lo) as usize) };
    }
}

/// The tick's bytes into `a`: every byte where `b` differs from the base `c`. Returns how
/// many bytes `a` had changed too (A != C there: a conflict, the tick won).
pub fn merge_bytes(a: &mut [u8], b: &[u8], c: &[u8]) -> usize {
    let mut conflicts = 0;
    let (ac, bc, cc) = (a.chunks_exact_mut(8), b.chunks_exact(8), c.chunks_exact(8));
    for ((a8, b8), c8) in ac.zip(bc).zip(cc) {
        let (bv, cv) = (u64::from_ne_bytes(b8.try_into().unwrap()), u64::from_ne_bytes(c8.try_into().unwrap()));
        if bv == cv {
            continue;
        }
        let av = u64::from_ne_bytes((&*a8).try_into().unwrap());
        // per byte: the tick's where it changed the byte
        let mut mask = 0u64;
        let d = bv ^ cv;
        for k in 0..8 {
            if d >> (8 * k) & 0xff != 0 {
                mask |= 0xff << (8 * k);
            }
        }
        let ad = (av ^ cv) & mask;
        for k in 0..8 {
            if ad >> (8 * k) & 0xff != 0 {
                conflicts += 1;
            }
        }
        a8.copy_from_slice(&((av & !mask) | (bv & mask)).to_ne_bytes());
    }
    let tail = a.len() / 8 * 8;
    for i in tail..a.len() {
        if b[i] != c[i] {
            if a[i] != c[i] {
                conflicts += 1;
            }
            a[i] = b[i];
        }
    }
    conflicts
}

/// What a merge did.
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Merged {
    /// pages the tick wrote and A did not: copied
    pub copied: usize,
    /// pages both wrote: merged by bytes
    pub merged: usize,
    /// bytes both changed (the tick's value kept)
    pub conflicts: usize,
}

/// Bring the tick's writes (B's marked pages, taken) into A: a page A has not written since
/// the fork is copied, one both wrote gets the bytes the tick changed (B against C). Guest
/// bytes in `skip` (the tick thread's stack below the fork) are left alone. The tick's pages
/// are marked in A afterwards, for the next `sync` to bring C (and B) up to date.
pub fn merge(w: &Worlds, ranges: &[(usize, usize)], skip: (u32, u32)) -> Merged {
    let mut pages = Vec::new();
    marked(w.b, ranges, true, &mut pages);
    let amap = w.a.pages();
    let mut m = Merged::default();
    let (slo, shi) = (skip.0 as usize, skip.1 as usize);
    for &p in &pages {
        let n = extent(ranges, p);
        let (lo, hi) = (p * PAGE, p * PAGE + n);
        // the part outside `skip`: [lo, hi) minus [slo, shi)
        let parts = if shi <= lo || slo >= hi {
            [(lo, hi), (hi, hi)]
        } else {
            [(lo, slo.max(lo)), (shi.min(hi), hi)]
        };
        let both = amap[p].load(Ordering::Relaxed) != 0 || (n > PAGE && amap[p + 1].load(Ordering::Relaxed) != 0);
        for (x, y) in parts {
            if x >= y {
                continue;
            }
            unsafe {
                if both {
                    let a = std::slice::from_raw_parts_mut(w.a.base.add(x), y - x);
                    let b = std::slice::from_raw_parts(w.b.base.add(x), y - x);
                    let c = std::slice::from_raw_parts(w.c.base.add(x), y - x);
                    m.conflicts += merge_bytes(a, b, c);
                } else {
                    std::ptr::copy_nonoverlapping(w.b.base.add(x), w.a.base.add(x), y - x);
                }
            }
        }
        if both {
            m.merged += 1;
        } else {
            m.copied += 1;
        }
        amap[p].store(1, Ordering::Release);
    }
    m
}

/// Bring B and C up to A on the pages A marked (taking the marks). Only while no tick runs.
/// Returns the number of pages copied.
pub fn sync(w: &Worlds, ranges: &[(usize, usize)]) -> usize {
    let mut pages = Vec::new();
    marked(w.a, ranges, true, &mut pages);
    copy_pages(w.a, &[w.b, w.c], ranges, &pages);
    pages.len()
}

/// The first copy: A's marks taken, then every mapped page copied into B and C (a write
/// during the copy is marked again and goes with the next `sync`); B's marks cleared.
pub fn initial(w: &Worlds) {
    let ranges = page_ranges(w.a);
    let mut pages = Vec::new();
    marked(w.a, &ranges, true, &mut pages);
    for &(lo, hi) in &ranges {
        copy_range(w.a, &[w.b, w.c], (lo * PAGE) as u32, (hi * PAGE).min(u32::MAX as usize + 1 - PAGE) as u32);
    }
    pages.clear();
    marked(w.b, &ranges, true, &mut pages);
}

/// `GUNMETAL_WORLD_VERIFY=1` (with the tick thread on): at most every 2 s, at Present on the
/// main thread, B and C are synced and compared with A over every mapped page but the main
/// thread's stack. A page that differs was written without a mark: logged with the first
/// differing address (the first call makes the initial copy).
pub fn verify_at_present() {
    static ON: OnceLock<bool> = OnceLock::new();
    if !*ON.get_or_init(|| std::env::var_os("GUNMETAL_WORLD_VERIFY").is_some()) {
        return;
    }
    let Some(w) = worlds() else { return };
    static LAST: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    let now = std::time::Instant::now();
    let Some(t) = *last else {
        initial(w);
        *last = Some(now);
        return;
    };
    if now.duration_since(t).as_millis() < 2000 {
        return;
    }
    *last = Some(now);
    let ranges = page_ranges(w.a);
    let t0 = std::time::Instant::now();
    let n = sync(w, &ranges);
    let synced = t0.elapsed();
    let (slo, shi) = (super::STACK_LO as usize / PAGE, super::STACK_HI as usize / PAGE);
    let mut bad = Vec::new();
    for &(lo, hi) in &ranges {
        for p in lo..hi {
            if (slo..shi).contains(&p) {
                continue;
            }
            let (a, b) = unsafe { (std::slice::from_raw_parts(at(w.a, p), PAGE), std::slice::from_raw_parts(at(w.b, p), PAGE)) };
            if a != b {
                let k = a.iter().zip(b).position(|(x, y)| x != y).unwrap_or(0);
                bad.push((p * PAGE + k) as u32);
                // the next round compares from equal pages again
                unsafe { std::ptr::copy_nonoverlapping(at(w.a, p), at(w.b, p), PAGE) };
                unsafe { std::ptr::copy_nonoverlapping(at(w.a, p), at(w.c, p), PAGE) };
            }
        }
    }
    let list: Vec<String> = bad.iter().take(24).map(|a| format!("{a:#x}")).collect();
    super::log!("world: synced {n} pages in {:.2} ms; {} pages differ unmarked: {}", synced.as_secs_f64() * 1e3, bad.len(), list.join(" "));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn worlds() -> Worlds {
        let m = || -> &'static Memory { Box::leak(Box::new(Memory::reserve_with(false))) };
        let (a, b, c) = (m(), m(), m());
        a.set_twins(vec![b, c]);
        a.map(0x10_0000, 0x10_4000);
        a.map(0x20_0000, 0x20_1000);
        Worlds { a, b, c }
    }

    #[test]
    fn twins_are_mapped_and_ranges_joined() {
        let w = worlds();
        w.a.map(0x10_4000, 0x10_5000);
        assert_eq!(page_ranges(w.a), vec![(0x100, 0x105), (0x200, 0x201)]);
        w.b.wr32(0x10_4ffc, 1);
        w.c.wr32(0x20_0000, 1);
    }

    #[test]
    fn sync_copies_written_pages_only() {
        let w = worlds();
        initial(&w);
        let r = page_ranges(w.a);
        w.a.wr32(0x10_1010, 0x1234_5678);
        unsafe { *w.a.base.add(0x10_2010) = 9 }; // unmarked: not copied
        assert_eq!(sync(&w, &r), 1);
        assert_eq!(w.b.rd32(0x10_1010), 0x1234_5678);
        assert_eq!(w.c.rd32(0x10_1010), 0x1234_5678);
        assert_eq!(w.b.rd8(0x10_2010), 0);
        assert_eq!(sync(&w, &r), 0);
    }

    #[test]
    fn a_marked_page_takes_the_start_of_the_next() {
        let w = worlds();
        initial(&w);
        let r = page_ranges(w.a);
        // a 4-byte store across the page end, marked by its first byte only
        unsafe { (w.a.base.add(0x10_0ffe) as *mut u32).write_unaligned(0xaabb_ccdd) };
        w.a.mark(0x10_0ffe, 1);
        sync(&w, &r);
        assert_eq!(w.b.rd32(0x10_0ffe), 0xaabb_ccdd);
        // the last page of a range has no spill
        w.a.wr32(0x20_0ffc, 7);
        sync(&w, &r);
        assert_eq!(w.c.rd32(0x20_0ffc), 7);
    }

    #[test]
    fn merge_copies_tick_pages_and_merges_shared_ones_by_bytes() {
        let w = worlds();
        w.a.wr32(0x10_0000, 1);
        w.a.wr32(0x10_2000, 0x1111_1111);
        initial(&w);
        let r = page_ranges(w.a);
        // the tick: a page only it writes, and one both write
        w.b.wr32(0x10_0000, 2);
        w.b.wr32(0x10_2000, 0x2222_1111); // changes the two high bytes
        // the drawing on the shared page: one byte the tick changed too, one it did not
        w.a.wr8(0x10_2003, 0x33);
        w.a.wr8(0x10_2008, 0x44);
        let m = merge(&w, &r, (0, 0));
        assert_eq!(m, Merged { copied: 1, merged: 1, conflicts: 1 });
        assert_eq!(w.a.rd32(0x10_0000), 2);
        assert_eq!(w.a.rd32(0x10_2000), 0x2222_1111); // the tick won its bytes
        assert_eq!(w.a.rd8(0x10_2008), 0x44); // the drawing kept its own
        // the next sync brings B and C to A
        sync(&w, &r);
        assert_eq!(w.c.rd32(0x10_0000), 2);
        assert_eq!(w.b.rd8(0x10_2008), 0x44);
        assert_eq!(w.c.rd32(0x10_2000), 0x2222_1111);
    }

    #[test]
    fn merge_leaves_the_skipped_range() {
        let w = worlds();
        initial(&w);
        let r = page_ranges(w.a);
        w.b.wr32(0x10_2000, 5);
        w.b.wr32(0x10_2800, 6);
        merge(&w, &r, (0x10_2000, 0x10_2800));
        assert_eq!(w.a.rd32(0x10_2000), 0);
        assert_eq!(w.a.rd32(0x10_2800), 6);
    }

    #[test]
    fn a_write_after_taking_the_marks_is_marked_again() {
        let w = worlds();
        initial(&w);
        let r = page_ranges(w.a);
        w.a.wr32(0x10_3000, 1);
        let mut p = Vec::new();
        marked(w.a, &r, true, &mut p);
        assert_eq!(p, vec![0x103]);
        w.a.wr32(0x10_3004, 2);
        p.clear();
        marked(w.a, &r, false, &mut p);
        assert_eq!(p, vec![0x103]);
    }

    #[test]
    fn frees_wait_while_a_tick_runs() {
        let q = FreeQueue::new();
        assert!(!q.defer(0x100));
        q.set_running(true);
        assert!(q.defer(0x100) && q.defer(0x200));
        q.set_running(false);
        assert_eq!(q.take(), vec![0x100, 0x200]);
        assert!(q.take().is_empty());
    }

    #[test]
    fn tick_rules() {
        assert_eq!(tick_rule("IDirect3DDevice9::SetLight"), TickRule::Record(2, 104));
        assert_eq!(tick_rule("IDirect3DDevice9::SetRenderState"), TickRule::Record(0, 0));
        assert_eq!(tick_rule("IDirect3DDevice9::DrawPrimitive"), TickRule::Unexpected);
        assert_eq!(tick_rule("IDirectSound3DBuffer::SetPosition"), TickRule::Live);
        assert_eq!(tick_rule("HeapAlloc"), TickRule::Live);
        assert_eq!(tick_rule("CreateFileA"), TickRule::Unexpected);
    }

    static SEEN: std::sync::Mutex<Vec<u32>> = std::sync::Mutex::new(Vec::new());

    fn fake_set_light(c: &mut crate::rt::Cpu) -> u32 {
        let m = crate::platform::mem();
        let (this, idx, p) = (crate::platform::arg(c, 0), crate::platform::arg(c, 1), crate::platform::arg(c, 2));
        let mut v = vec![this, idx];
        v.extend((0..26).map(|k| m.rd32(p + 4 * k)));
        *SEEN.lock().unwrap() = v;
        0
    }

    #[test]
    fn a_recorded_set_light_replays_the_same_bytes() {
        let m = crate::platform::init_memory();
        let k = crate::platform::thunk_index(crate::platform::thunk("IDirect3DDevice9::SetLight", 3, fake_set_light)).unwrap();
        let light = crate::platform::alloc(104);
        for j in 0..26 {
            m.wr32(light + 4 * j, 0x100 + j);
        }
        let rule = tick_rule("IDirect3DDevice9::SetLight");
        record(k, vec![0xabc, 3, light], rule, m);
        // the light changes after the call: the copy counts
        m.wr32(light, 0);
        let calls: Vec<Call> = take_calls().into_iter().filter(|c| c.k == k).collect();
        assert_eq!(calls.len(), 1);
        crate::platform::replay_calls(&calls);
        let seen = SEEN.lock().unwrap().clone();
        assert_eq!(seen[..2], [0xabc, 3]);
        assert_eq!(seen[2..], (0..26).map(|j| 0x100 + j).collect::<Vec<u32>>()[..]);
    }

    #[test]
    fn merge_bytes_counts_conflicts() {
        let c = [0u8; 11];
        let mut a = c;
        let mut b = c;
        b[1] = 1;
        b[10] = 2;
        a[1] = 3;
        a[2] = 4;
        a[10] = 5;
        assert_eq!(merge_bytes(&mut a, &b, &c), 2);
        assert_eq!(a, [0, 1, 4, 0, 0, 0, 0, 0, 0, 0, 2]);
    }
}
