//! DirectSound structures between the game's 32-bit layout and the host's. Needs no game files
//! and no sound device.
#![cfg(windows)]

use rust_metal::platform::dsound::{buffer_desc_from_guest, guest_lock_parts, notify_positions_from_guest};
use rust_metal::platform::{alloc, init_memory};

#[test]
fn buffer_desc() {
    let m = init_memory();
    // WAVEFORMATEX: PCM, mono, 22050 Hz, 44100 bytes/s, block 2, 16 bits, no extra bytes
    let wfx = alloc(18);
    for (off, v) in [(0, 1u16), (2, 1), (12, 2), (14, 16), (16, 0)] {
        m.wr16(wfx + off, v);
    }
    m.wr32(wfx + 4, 22050);
    m.wr32(wfx + 8, 44100);
    // DSBUFFERDESC: 36 bytes, lpwfxFormat at 16, guid3DAlgorithm at 20
    let d = alloc(36);
    m.slice(d, 36).fill(0);
    m.wr32(d, 36);
    m.wr32(d + 4, 0x100e0);
    m.wr32(d + 8, 0x6000);
    m.wr32(d + 16, wfx);
    m.slice(d + 20, 16).copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]);
    let (desc, format) = buffer_desc_from_guest(m, d);
    assert_eq!(desc.dwSize as usize, std::mem::size_of_val(&desc));
    assert_eq!((desc.dwFlags, desc.dwBufferBytes, desc.dwReserved), (0x100e0, 0x6000, 0));
    assert_eq!(desc.guid3DAlgorithm, windows::core::GUID::from_u128(0x04030201_0605_0807_090a_0b0c0d0e0f10));
    // the format is the game's, in place
    assert_eq!(desc.lpwfxFormat as usize, m.base as usize + wfx as usize);
    let (tag, channels, rate, bytes, align, bits) =
        (format.wFormatTag, format.nChannels, format.nSamplesPerSec, format.nAvgBytesPerSec, format.nBlockAlign, format.wBitsPerSample);
    assert_eq!((tag, channels, rate, bytes, align, bits), (1, 1, 22050, 44100, 2, 16));

    // the primary buffer: no format
    m.wr32(d + 4, 0x11);
    m.wr32(d + 8, 0);
    m.wr32(d + 16, 0);
    let (desc, _) = buffer_desc_from_guest(m, d);
    assert!(desc.lpwfxFormat.is_null());
    assert_eq!((desc.dwFlags, desc.dwBufferBytes), (0x11, 0));
}

#[test]
fn notify_positions() {
    let m = init_memory();
    // the game's music stream: three positions, one event
    let a = alloc(24);
    for k in 0..3 {
        m.wr32(a + 8 * k, 0x1fff + 0x2000 * k);
        m.wr32(a + 8 * k + 4, 0x234);
    }
    let v = notify_positions_from_guest(m, a, 3, |g| if g == 0x234 { 0x5678 } else { 0 });
    assert_eq!(v.len(), 3);
    for (k, p) in v.iter().enumerate() {
        assert_eq!(p.dwOffset, 0x1fff + 0x2000 * k as u32);
        assert_eq!(p.hEventNotify.0 as isize, 0x5678);
    }
}

#[test]
fn lock_parts() {
    // the shadow holds the whole buffer: the first part at the lock's offset, a wrapped second
    // part at the start
    assert_eq!(guest_lock_parts(0x1000, 0x5000, true), (0x6000, 0x1000));
    assert_eq!(guest_lock_parts(0x1000, 0x2000, false), (0x3000, 0));
    assert_eq!(guest_lock_parts(0x1000, 0, false), (0x1000, 0));
}

#[test]
fn notification_sets_the_guest_event() {
    use rust_metal::platform::{dsound::host_event, kernel32};
    use rust_metal::rt::{Cpu, ESP};
    let m = init_memory();
    // CreateEventA(no attributes, manual reset, not set, no name), as the game's music stream
    let stack = alloc(64);
    m.slice(stack, 64).fill(0);
    m.wr32(stack + 8, 1);
    let mut c = Cpu::new(m);
    c.r[ESP] = stack;
    let create = kernel32::IMPORTS.iter().find(|i| i.0 == "CreateEventA").unwrap().2;
    let guest = create(&mut c);
    let host = host_event(guest);
    assert_ne!(host, 0);
    assert_eq!(host_event(guest), host, "one host event per guest event");
    assert_eq!(kernel32::wait(&[guest], false, 0), kernel32::WAIT_TIMEOUT);
    // DirectSound reaching a position signals the host event
    unsafe { windows_sys::Win32::System::Threading::SetEvent(host as _) };
    assert_eq!(kernel32::wait(&[guest], false, 2000), 0);
    // manual reset: set until the game resets it
    assert_eq!(kernel32::wait(&[guest], false, 0), 0);
}
