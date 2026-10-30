//! Local time: the game stamps its save slots with GetLocalTime; it must be the host's local
//! time, consistent with the time zone and FileTimeToLocalFileTime. Needs no game files.
#![cfg(windows)]

use rust_metal::platform::{alloc, init_memory, kernel32};
use rust_metal::rt::{Cpu, ESP};

/// Call kernel32's import `name` with arguments on a guest stack.
fn call(name: &str, args: &[u32]) -> u32 {
    let m = init_memory();
    let stack = alloc(256);
    let mut c = Cpu::new(m);
    c.r[ESP] = stack + 64;
    for (n, &a) in args.iter().enumerate() {
        m.wr32(stack + 68 + 4 * n as u32, a);
    }
    let f = kernel32::IMPORTS.iter().find(|i| i.0 == name).unwrap_or_else(|| panic!("no import {name}")).2;
    let r = f(&mut c);
    rust_metal::platform::free(stack);
    r
}

/// A SYSTEMTIME at `p` as a FILETIME (through the guest's SystemTimeToFileTime).
fn file_time(p: u32) -> u64 {
    let ft = alloc(8);
    assert_eq!(call("SystemTimeToFileTime", &[p, ft]), 1);
    let m = init_memory();
    m.rd32(ft) as u64 | (m.rd32(ft + 4) as u64) << 32
}

const SECOND: i64 = 10_000_000;

#[test]
fn local_time_is_the_hosts() {
    init_memory();
    let p = alloc(16);
    call("GetLocalTime", &[p]);
    let guest = file_time(p) as i64;
    let mut st = unsafe { std::mem::zeroed() };
    unsafe { windows_sys::Win32::System::SystemInformation::GetLocalTime(&mut st) };
    let m = init_memory();
    let host = alloc(16);
    for (k, v) in [st.wYear, st.wMonth, st.wDayOfWeek, st.wDay, st.wHour, st.wMinute, st.wSecond, st.wMilliseconds].into_iter().enumerate() {
        m.wr16(host + 2 * k as u32, v);
    }
    assert!((guest - file_time(host) as i64).abs() < 2 * SECOND, "GetLocalTime is not the host's local time");
}

#[test]
fn local_time_matches_local_file_time() {
    init_memory();
    let (utc, local) = (alloc(16), alloc(16));
    call("GetSystemTime", &[utc]);
    call("GetLocalTime", &[local]);
    let (ft, lft) = (alloc(8), alloc(8));
    let v = file_time(utc);
    let m = init_memory();
    m.wr32(ft, v as u32);
    m.wr32(ft + 4, (v >> 32) as u32);
    assert_eq!(call("FileTimeToLocalFileTime", &[ft, lft]), 1);
    let converted = m.rd32(lft) as u64 | (m.rd32(lft + 4) as u64) << 32;
    assert!((converted as i64 - file_time(local) as i64).abs() < 2 * SECOND);
    // GetTimeZoneInformation's bias (minutes, UTC = local + bias) agrees, up to daylight time
    let tz = alloc(172);
    let id = call("GetTimeZoneInformation", &[tz]);
    let bias = m.rd32(tz) as i32 as i64
        + match id {
            1 => m.rd32(tz + 84) as i32 as i64,  // standard bias
            2 => m.rd32(tz + 168) as i32 as i64, // daylight bias
            _ => 0,
        };
    assert!((v as i64 - bias * 60 * SECOND - file_time(local) as i64).abs() < 2 * SECOND);
}
