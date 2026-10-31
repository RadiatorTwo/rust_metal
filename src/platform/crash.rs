//! Reports for the game's fatal C runtime errors, which otherwise end the run with only a
//! message box ("R6025 pure virtual function call").
//!
//! The C runtime ends the process through `__amsg_exit(code)` (0x588d97): it shows the error
//! ("R60xx") and exits with 255. `_purecall` calls it with 25 when a virtual method is called
//! on an object whose vtable is still or again its abstract base's: an object being built or
//! destroyed, typically one thread using an object that another is destroying. Our hook writes
//! to the log what is needed to find the race and then runs the original:
//! - the thread (the main thread, or the guest thread with its start address) and `ecx`, the
//!   object the virtual method was called on, with its vtable;
//! - the return-address chain: the frame-pointer chain (`ebp_chain`) and, since much of the
//!   game is compiled without frame pointers, a scan of the stack for dwords that are return
//!   addresses (`return_scan`: code addresses right after a call instruction); each with the
//!   recompiled function it lies in, named from data/symbols.txt where known;
//! - every guest thread's last import (name and return address) and the chain of its stack
//!   from there: the other threads are stopped in imports while this one has the turn.
//!
//! Each guest thread registers itself (`register`, a free slot of 64, freed by `unregister`);
//! imports note themselves per thread (`note_import`: the slot from a thread-local, a few
//! relaxed stores; unregistered host threads note nothing). Every guest read checks that the
//! memory is there (`guest_readable`), so a bad pointer does not take the report down.

use super::{log, mem, thunk_name};
use crate::rt::{Cpu, EBP, ECX, ESP, call_original};
use std::ops::Range;
use std::sync::atomic::{AtomicU32, Ordering};

pub const AMSG_EXIT: u32 = 0x588d97;

/// The game's code (.text).
const TEXT: Range<u32> = 0x401000..0x5e3315;

/// What made a guest thread.
#[derive(Clone, Copy, PartialEq, Debug)]
#[repr(u32)]
pub enum Kind {
    Main = 1,
    /// CreateThread (its start routine)
    Thread = 2,
    /// a timeSetEvent timer thread (its callback)
    Timer = 3,
}

/// One guest thread as the report knows it (all 0: no thread).
struct Slot {
    tid: AtomicU32,
    kind: AtomicU32,
    start: AtomicU32,
    tib: AtomicU32,
    /// thunk index + 1 of its last import, its return address, esp and ebp at the call
    import: AtomicU32,
    ret: AtomicU32,
    esp: AtomicU32,
    ebp: AtomicU32,
}

impl Slot {
    const fn new() -> Slot {
        const Z: AtomicU32 = AtomicU32::new(0);
        Slot { tid: Z, kind: Z, start: Z, tib: Z, import: Z, ret: Z, esp: Z, ebp: Z }
    }
}

const SLOTS: usize = 64;
static THREADS: [Slot; SLOTS] = [const { Slot::new() }; SLOTS];

thread_local! {
    /// The calling thread's slot (None: a host thread that did not register, e.g. a host-event
    /// forwarder: its imports are not noted, so it never overwrites a guest thread's).
    static MINE: std::cell::Cell<Option<&'static Slot>> = const { std::cell::Cell::new(None) };
}

/// The calling thread's slot, if it registered.
fn slot() -> Option<&'static Slot> {
    MINE.with(|m| m.get())
}

/// The calling host thread runs guest code from now on: as `kind` from `start`, with its
/// thread information block at `tib`. It takes a free slot (the first whose tid is 0); when
/// all 64 are taken it is not reported.
pub fn register(kind: Kind, start: u32, tib: u32) {
    let tid = super::kernel32::thread_id();
    let Some(s) = THREADS.iter().find(|s| s.tid.compare_exchange(0, tid, Ordering::Relaxed, Ordering::Relaxed).is_ok()) else {
        return;
    };
    s.kind.store(kind as u32, Ordering::Relaxed);
    s.start.store(start, Ordering::Relaxed);
    s.tib.store(tib, Ordering::Relaxed);
    s.import.store(0, Ordering::Relaxed);
    MINE.with(|m| m.set(Some(s)));
}

/// The calling thread has finished running guest code: its slot is free again.
pub fn unregister() {
    if let Some(s) = MINE.with(|m| m.take()) {
        s.kind.store(0, Ordering::Relaxed);
        s.import.store(0, Ordering::Relaxed);
        s.tib.store(0, Ordering::Relaxed);
        s.tid.store(0, Ordering::Release);
    }
}

/// The stacks [lo, hi) of the guest threads other than the main thread.
pub fn thread_stacks() -> Vec<(u32, u32)> {
    let m = mem();
    THREADS
        .iter()
        .filter(|s| matches!(s.kind.load(Ordering::Relaxed), 2 | 3))
        .map(|s| s.tib.load(Ordering::Relaxed))
        .filter(|&t| t != 0)
        .map(|t| (m.rd32(t + 8), m.rd32(t + 4)))
        .collect()
}

/// The calling thread calls import `k` (its return address on the guest stack).
pub fn note_import(c: &Cpu, k: usize) {
    let Some(s) = slot() else { return };
    s.import.store(k as u32 + 1, Ordering::Relaxed);
    s.ret.store(mem().rd32(c.r[ESP]), Ordering::Relaxed);
    s.esp.store(c.r[ESP], Ordering::Relaxed);
    s.ebp.store(c.r[EBP], Ordering::Relaxed);
}

/// The return addresses of the frame-pointer chain from `ebp`: each frame holds the caller's
/// ebp at [ebp] and the return address at [ebp + 4]. A frame must lie inside the stack
/// [lo, hi), be 4-aligned and above the one before; it ends at a return address of 0, at
/// unreadable memory (`rd` gives None) and after `max` frames.
pub fn ebp_chain(rd: &dyn Fn(u32) -> Option<u32>, mut ebp: u32, lo: u32, hi: u32, max: usize) -> Vec<u32> {
    let mut out = Vec::new();
    while out.len() < max && ebp >= lo && ebp % 4 == 0 && ebp.checked_add(8).is_some_and(|e| e <= hi) {
        let Some(ret) = rd(ebp + 4).filter(|&r| r != 0) else { break };
        out.push(ret);
        match rd(ebp) {
            Some(next) if next > ebp => ebp = next,
            _ => break,
        }
    }
    out
}

/// Whether `a` follows a call instruction (`rd8`: code bytes, None where there are none):
/// call rel32 (e8), call reg / [reg] (ff /2 without and with an 8- or 32-bit displacement, with
/// or without a SIB byte), call [abs32] (ff 15).
pub fn looks_like_return(rd8: &dyn Fn(u32) -> Option<u8>, a: u32) -> bool {
    let b = |back: u32| a.checked_sub(back).and_then(rd8);
    let ff2 = |back: u32, ok: &dyn Fn(u8) -> bool| b(back) == Some(0xff) && b(back - 1).is_some_and(ok);
    let reg = |m: u8| m >> 3 & 7 == 2;
    b(5) == Some(0xe8)
        || ff2(2, &|m| reg(m) && (m >> 6 == 3 || (m >> 6 == 0 && m & 7 != 4 && m & 7 != 5)))
        || ff2(3, &|m| reg(m) && ((m >> 6 == 1 && m & 7 != 4) || (m >> 6 == 0 && m & 7 == 4)))
        || ff2(4, &|m| reg(m) && m >> 6 == 1 && m & 7 == 4)
        || ff2(6, &|m| reg(m) && ((m >> 6 == 2 && m & 7 != 4) || m == 0x15))
        || ff2(7, &|m| reg(m) && m >> 6 == 2 && m & 7 == 4)
}

/// The dwords of the stack from `esp` (at most `words`, below `hi`) that are return addresses
/// into `text` (`looks_like_return`), as (stack address, value); at most `max` of them.
pub fn return_scan(
    rd: &dyn Fn(u32) -> Option<u32>,
    rd8: &dyn Fn(u32) -> Option<u8>,
    esp: u32,
    hi: u32,
    text: Range<u32>,
    words: u32,
    max: usize,
) -> Vec<(u32, u32)> {
    (0..words)
        .map(|k| esp.wrapping_add(4 * k))
        .take_while(|&a| a >= esp && a.checked_add(4).is_some_and(|e| e <= hi))
        .filter_map(|a| Some((a, rd(a)?)))
        .filter(|&(_, v)| text.contains(&v) && looks_like_return(rd8, v))
        .take(max)
        .collect()
}

/// The recompiled function `a` lies in (the nearest entry at or below it), named if known.
fn function_of(a: u32) -> String {
    let Some(d) = crate::rt::DISPATCH.get() else { return "?".into() };
    let k = d.table.partition_point(|e| e.0 <= a);
    match k.checked_sub(1).map(|k| d.table[k].0) {
        Some(f) => format!("{a:#x} in {}", crate::image::named(f)),
        None => format!("{a:#x}"),
    }
}

/// The chain and the stack scan of a thread with these registers and TIB, one line each.
fn stack_report(esp: u32, ebp: u32, tib: u32) -> Vec<String> {
    let m = mem();
    let (lo, hi) = if tib != 0 { (m.rd32(tib + 8), m.rd32(tib + 4)) } else { (esp, esp) };
    if !(lo..hi).contains(&esp) {
        return vec![format!("    (esp {esp:#x} outside its stack {lo:#x}..{hi:#x})")];
    }
    let rd = |a: u32| (a >= lo && a.checked_add(4).is_some_and(|e| e <= hi) && super::guest_readable(a, 4)).then(|| m.rd32(a));
    let rd8 = |a: u32| TEXT.contains(&a).then(|| m.rd8(a));
    let mut out = vec![format!("    esp {esp:#x} ebp {ebp:#x}, stack {lo:#x}..{hi:#x}")];
    let chain = ebp_chain(&rd, ebp, lo, hi, 24);
    out.push(format!("    ebp chain: {}", chain.iter().map(|&r| function_of(r)).collect::<Vec<_>>().join(" <- ")));
    for (at, r) in return_scan(&rd, &rd8, esp, hi, TEXT, 4096, 32) {
        out.push(format!("    [esp+{:#x}] {}", at - esp, function_of(r)));
    }
    out
}

fn thread_line(s: &Slot) -> String {
    let kind = match s.kind.load(Ordering::Relaxed) {
        1 => "main",
        2 => "CreateThread",
        3 => "timer",
        _ => "?",
    };
    format!("thread {} ({kind}, start {})", s.tid.load(Ordering::Relaxed), crate::image::named(s.start.load(Ordering::Relaxed)))
}

/// The game's step of its main loop (WinMain 0x57a9b0): false makes WinMain send WM_CLOSE
/// and shut down (0x45c430).
pub const FRAME_STEP: u32 = 0x45c770;
/// The loader thread's state for the main thread (thiscall): 1 still loading, 2 loaded,
/// 3/5 failed, 6 unknown state.
pub const LOADER_POLL: u32 = 0x4eacf0;
/// WinMain's shutdown (after its loop).
pub const SHUTDOWN: u32 = 0x45c430;
/// The game's window procedure (stdcall hwnd, msg, wparam, lparam).
pub const WND_PROC: u32 = 0x57ae70;
/// Starts the loader thread 0x4eadf0 (thiscall, 5 arguments).
pub const LOADER_START: u32 = 0x4eac60;

pub fn wnd_proc(c: &mut Cpu) {
    let msg = mem().rd32(c.r[ESP] + 8);
    if matches!(msg, 0x2 | 0x10 | 0x12 | 0x16) {
        log!("game: window message {msg:#x} at {} ms (from {:#x})", super::kernel32::ms_since_start(), super::LAST_CALL.load(Ordering::Relaxed));
    }
    call_original(c, WND_PROC);
}

pub fn loader_start(c: &mut Cpu) {
    log!("game: loader thread started at {} ms", super::kernel32::ms_since_start());
    call_original(c, LOADER_START);
}

pub fn frame_step(c: &mut Cpu) {
    call_original(c, FRAME_STEP);
    if c.r[crate::rt::EAX] & 0xff == 0 {
        log!("game: the main loop's step 0x45c770 returned false at {} ms: the game quits", super::kernel32::ms_since_start());
    }
}

pub fn loader_poll(c: &mut Cpu) {
    call_original(c, LOADER_POLL);
    let r = c.r[crate::rt::EAX];
    if r >= 2 {
        log!("game: the loader thread's state {r} at {} ms", super::kernel32::ms_since_start());
    }
}

pub fn shutdown(c: &mut Cpu) {
    log!("game: shutdown (0x45c430) at {} ms", super::kernel32::ms_since_start());
    call_original(c, SHUTDOWN);
}

/// __amsg_exit(code): the report, then the original (its message box and exit(255)).
pub fn amsg_exit(c: &mut Cpu) {
    let m = mem();
    // a dword of guest memory that may not be there (ecx is arbitrary for most codes)
    let rd = |a: u32| super::guest_readable(a, 4).then(|| m.rd32(a));
    let code = c.r[ESP].checked_add(4).and_then(rd).unwrap_or(u32::MAX);
    let what = if code == 25 { " pure virtual function call" } else { "" };
    let me = slot();
    let who = me.map_or("an unregistered host thread".to_string(), thread_line);
    log!("fatal: the C runtime's error R60{code:02}{what} (__amsg_exit({code})) on {who}");
    let this = c.r[ECX];
    let vt = rd(this).map_or("(not readable)".to_string(), |v| format!("{v:#x}"));
    log!("  ecx (the object, if a virtual call) {this:#x}, its first dword (vtable) {vt}");
    log!("  last import call returned to {:#x}", super::LAST_CALL.load(Ordering::Relaxed));
    for line in stack_report(c.r[ESP], c.r[EBP], c.fs_base) {
        log!("{line}");
    }
    let mine = me.map(|s| s as *const Slot);
    for s in THREADS.iter().filter(|s| s.kind.load(Ordering::Relaxed) != 0) {
        let k = s.import.load(Ordering::Relaxed);
        let import = if k == 0 { "none".to_string() } else { thunk_name(k as usize - 1) };
        log!("  {}: last import {import}, returning to {}", thread_line(s), function_of(s.ret.load(Ordering::Relaxed)));
        if mine != Some(s as *const Slot) && k != 0 {
            for line in stack_report(s.esp.load(Ordering::Relaxed), s.ebp.load(Ordering::Relaxed), s.tib.load(Ordering::Relaxed)) {
                log!("{line}");
            }
        }
    }
    call_original(c, AMSG_EXIT);
}
