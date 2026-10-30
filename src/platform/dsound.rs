//! dsound.dll, passed through to the system's. The game imports only DirectSoundCreate8
//! (ordinal 11); every interface it gets is a guest COM object (`com::wrap`) whose vtable is
//! ours, in the SDK's method order, and each method calls the host interface behind `this`,
//! as `d3d9/` does. Structures without pointers (DSCAPS, DSBCAPS, WAVEFORMATEX, DS3DBUFFER,
//! DS3DLISTENER) are passed in place; DSBUFFERDESC (a format pointer) and DSBPOSITIONNOTIFY (an
//! event handle) are converted. Methods we do not implement return 0 and are logged at their
//! first call.
//!
//! What the game does (`data/symbols.txt`): sound_init (0x465130), twice at start-up (two
//! sound objects, two DirectSound devices): DirectSoundCreate8 on the default device, GetCaps,
//! SetCooperativeLevel(its window, DSSCL_PRIORITY), the primary buffer (PRIMARYBUFFER |
//! CTRL3D), SetFormat to 44100 Hz 16-bit stereo (by its settings), the primary's
//! IDirectSound3DListener, Play(LOOPING) on the primary, and a 1 s buffer filled once and
//! played looping (0x4672a0). Sound effects: one secondary buffer per sample (0x465550, flags
//! LOCSOFTWARE | CTRLFREQUENCY | CTRLVOLUME, 22050 Hz 16-bit mono in the front end), filled
//! once with Lock(ENTIREBUFFER)/Unlock; per voice a DuplicateSoundBuffer (0x465bc0, with an
//! IDirectSound3DBuffer for 3D sounds), SetVolume, SetFrequency, SetCurrentPosition, Play,
//! GetStatus. The listener's CommitDeferredSettings every frame. A streamed buffer
//! (stream_open 0x4688d0; not seen in the front end) is 3 x 0x2000 bytes (flags 0x10180:
//! GETCURRENTPOSITION2 | CTRLPOSITIONNOTIFY | CTRLVOLUME) with IDirectSoundNotify positions at
//! the end of each third, all on one event; its thread (0x469300) waits for that event with
//! WaitForMultipleObjects and refills the third that has played.
//!
//! Lock: the game writes the locked parts by guest address. Each buffer has one guest shadow
//! of its whole size (`com::shadow`); a lock's first part is at its offset in it, a wrapped
//! second part at its start, the same split as the host's lock. The host's bytes are copied in
//! at Lock and the game's back at Unlock (DirectSound's buffers are ordinary memory).
//!
//! Notifications: the game's events are kernel32's guest objects, which DirectSound cannot
//! signal. Each guest event the game hands to SetNotificationPositions gets a host event and
//! a host thread that waits for it and sets the guest event (`host_event`); the thread ends
//! when the guest event is closed.

use super::d3d9::types::read_guest;
use super::d3d9::{add_ref, cp, gp, hr, release, vtbl};
use super::{Import, arg, com, kernel32, lock, log, mem, method_not_implemented as NI};
use crate::rt::{Cpu, Memory};
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::Mutex;
use windows::Win32::Foundation::{HANDLE, HWND};
use windows::Win32::Media::Audio::DirectSound::*;
use windows::Win32::Media::Audio::WAVEFORMATEX;
use windows::core::{GUID, IUnknown, Interface};

const E_NOINTERFACE: u32 = 0x8000_4002;
const DSERR_INVALIDPARAM: u32 = 0x8007_0057;

/// Bytes of the game's DSBUFFERDESC: 5 dwords (lpwfxFormat a guest pointer at 16) and
/// guid3DAlgorithm at 20.
pub const BUFFER_DESC_SIZE: u32 = 36;

/// The game's DSBUFFERDESC at `a` for the host, and a copy of its format (default without
/// one: the primary buffer). The host's lpwfxFormat points at the game's WAVEFORMATEX in place
/// (guest memory is host memory; the structure has no pointers).
pub fn buffer_desc_from_guest(m: &Memory, a: u32) -> (DSBUFFERDESC, WAVEFORMATEX) {
    let d = |k: u32| m.rd32(a + 4 * k);
    let format = d(4);
    let mut desc = DSBUFFERDESC {
        dwSize: std::mem::size_of::<DSBUFFERDESC>() as u32,
        dwFlags: d(1),
        dwBufferBytes: d(2),
        dwReserved: d(3),
        lpwfxFormat: gp(format),
        ..Default::default()
    };
    // DirectX 3's DSBUFFERDESC1 ends before the 3D algorithm
    if d(0) >= BUFFER_DESC_SIZE {
        desc.guid3DAlgorithm = read_guest(m, a + 20);
    }
    let wfx = if format != 0 { read_guest(m, format) } else { WAVEFORMATEX::default() };
    (desc, wfx)
}

/// The game's `n` DSBPOSITIONNOTIFY at `a` (offset, guest event: 8 bytes each), the events
/// mapped to host handles by `event`.
pub fn notify_positions_from_guest(m: &Memory, a: u32, n: u32, event: impl Fn(u32) -> isize) -> Vec<DSBPOSITIONNOTIFY> {
    (0..n)
        .map(|k| DSBPOSITIONNOTIFY { dwOffset: m.rd32(a + 8 * k), hEventNotify: HANDLE(event(m.rd32(a + 8 * k + 4)) as *mut c_void) })
        .collect()
}

/// Guest addresses of a lock's parts in the buffer's shadow at `shadow`: the first at the
/// lock's `offset`, the wrapped second one (if any) at the start.
pub fn guest_lock_parts(shadow: u32, offset: u32, wrapped: bool) -> (u32, u32) {
    (shadow + offset, if wrapped { shadow } else { 0 })
}

// ---- notification events ----

/// Host events standing for guest events, by guest handle.
static EVENTS: Mutex<Option<HashMap<u32, usize>>> = Mutex::new(None);

/// The host event that DirectSound signals for guest event `guest`: made at the first use
/// (auto-reset), with a thread that sets the guest event whenever the host one is signalled.
/// Setting a guest event from a host thread is safe (kernel32's events are host-side state,
/// waits on them hold no turn). The thread looks every second whether the
/// guest event still exists and ends when it is gone; the host event is then left open, as
/// DirectSound may still hold it for a buffer the game has not released.
pub fn host_event(guest: u32) -> isize {
    use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
    use windows_sys::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
    let mut g = lock(&EVENTS);
    let map = g.get_or_insert_with(HashMap::new);
    if let Some(&h) = map.get(&guest) {
        return h as isize;
    }
    let h = unsafe { CreateEventW(std::ptr::null(), 0, 0, std::ptr::null()) } as usize;
    if h == 0 {
        log!("warn: dsound: no host event for guest event {guest:#x}");
        return 0;
    }
    map.insert(guest, h);
    std::thread::spawn(move || {
        loop {
            let signalled = unsafe { WaitForSingleObject(h as _, 1000) } == WAIT_OBJECT_0;
            if !kernel32::is_event(guest) || (signalled && kernel32::set_event_state(guest, true) == 0) {
                break;
            }
        }
        if let Some(m) = lock(&EVENTS).as_mut() {
            m.remove(&guest);
        }
    });
    h as isize
}

// ---- argument helpers ----

fn ptr<T>(c: &Cpu, k: u32) -> *mut T {
    gp(arg(c, k))
}
fn cptr<T>(c: &Cpu, k: u32) -> *const T {
    cp(arg(c, k))
}
fn u(c: &Cpu, k: u32) -> u32 {
    arg(c, k)
}
fn i(c: &Cpu, k: u32) -> i32 {
    arg(c, k) as i32
}
fn f(c: &Cpu, k: u32) -> f32 {
    f32::from_bits(arg(c, k))
}

/// A method that passes its arguments through, each converted by a helper above.
macro_rules! pass {
    ($name:ident: $vt:ty, $field:ident $(, $conv:ident $k:literal)*) => {
        fn $name(c: &mut Cpu) -> u32 {
            let p = com::host(arg(c, 0));
            hr(unsafe { (vtbl::<$vt>(p).$field)(p $(, $conv(c, $k))*) })
        }
    };
}

/// QueryInterface for any of our interfaces: an interface we wrap is asked of the host and
/// wrapped (IUnknown with the table of the object asked, `own`); anything else E_NOINTERFACE.
fn query_interface(c: &mut Cpu, own: &'static [Import]) -> u32 {
    let (this, riid, ppv) = (arg(c, 0), arg(c, 1), arg(c, 2));
    let iid: GUID = read_guest(mem(), riid);
    let table = match iid {
        _ if iid == IUnknown::IID => own,
        _ if iid == IDirectSound::IID || iid == IDirectSound8::IID => IDIRECTSOUND8,
        _ if iid == IDirectSoundBuffer::IID || iid == IDirectSoundBuffer8::IID => BUFFER,
        _ if iid == IDirectSound3DBuffer::IID => BUFFER_3D,
        _ if iid == IDirectSound3DListener::IID => LISTENER,
        _ if iid == IDirectSoundNotify::IID => NOTIFY,
        _ => {
            log!("warn: dsound: QueryInterface({iid:?}): not wrapped (E_NOINTERFACE)");
            if ppv != 0 {
                mem().wr32(ppv, 0);
            }
            return E_NOINTERFACE;
        }
    };
    let p = com::host(this);
    let mut out: *mut c_void = std::ptr::null_mut();
    let r = unsafe { (vtbl::<windows::core::IUnknown_Vtbl>(p).QueryInterface)(p, &iid, &mut out) };
    mem().wr32(ppv, if r.is_ok() { com::wrap(table, out) } else { 0 });
    hr(r)
}

// ---- DirectSoundCreate8 and IDirectSound8 ----

/// DirectSoundCreate8(device GUID, out, outer): the system's IDirectSound8.
fn direct_sound_create8(c: &mut Cpu) -> u32 {
    let (guid, out) = (arg(c, 0), arg(c, 1));
    let mut ds: Option<IDirectSound8> = None;
    let device = (guid != 0).then(|| cptr::<GUID>(c, 0));
    let r = unsafe { DirectSoundCreate8(device, &mut ds, None::<&IUnknown>) };
    match (r, ds) {
        (Ok(()), Some(ds)) => {
            log!("dsound: DirectSoundCreate8: the system's default device");
            mem().wr32(out, com::wrap(IDIRECTSOUND8, ds.into_raw()));
            0
        }
        (r, _) => {
            let e = r.err().map_or(DSERR_INVALIDPARAM, |e| e.code().0 as u32);
            log!("warn: DirectSoundCreate8 failed: {e:#x} (no sound)");
            if out != 0 {
                mem().wr32(out, 0);
            }
            e
        }
    }
}

fn ds_qi(c: &mut Cpu) -> u32 {
    query_interface(c, IDIRECTSOUND8)
}

/// CreateSoundBuffer(desc, out, outer).
fn create_sound_buffer(c: &mut Cpu) -> u32 {
    let p = com::host(arg(c, 0));
    let out = arg(c, 2);
    let (desc, wfx) = buffer_desc_from_guest(mem(), arg(c, 1));
    let mut b: *mut c_void = std::ptr::null_mut();
    let r = unsafe { (vtbl::<IDirectSound_Vtbl>(p).CreateSoundBuffer)(p, &desc, &mut b, std::ptr::null_mut()) };
    if super::trace_api() || r.is_err() {
        let (rate, bits, channels) = (wfx.nSamplesPerSec, wfx.wBitsPerSample, wfx.nChannels);
        log!("dsound: CreateSoundBuffer(flags {:#x}, {} bytes, {rate} Hz {bits} bits x{channels}) -> {:#x}", desc.dwFlags, desc.dwBufferBytes, r.0);
    }
    mem().wr32(out, if r.is_ok() { com::wrap(BUFFER, b) } else { 0 });
    hr(r)
}

/// DuplicateSoundBuffer(original, out): a second voice sharing the original's data.
fn duplicate_sound_buffer(c: &mut Cpu) -> u32 {
    let p = com::host(arg(c, 0));
    let (original, out) = (com::host(arg(c, 1)), arg(c, 2));
    let mut b: *mut c_void = std::ptr::null_mut();
    let r = unsafe { (vtbl::<IDirectSound_Vtbl>(p).DuplicateSoundBuffer)(p, original, &mut b) };
    mem().wr32(out, if r.is_ok() { com::wrap(BUFFER, b) } else { 0 });
    hr(r)
}

/// SetCooperativeLevel(window, level): the game's window is our host window.
fn set_cooperative_level(c: &mut Cpu) -> u32 {
    let p = com::host(arg(c, 0));
    let hwnd = HWND(super::window::host_hwnd(arg(c, 1)) as *mut c_void);
    hr(unsafe { (vtbl::<IDirectSound_Vtbl>(p).SetCooperativeLevel)(p, hwnd, arg(c, 2)) })
}

pass!(ds_get_caps: IDirectSound_Vtbl, GetCaps, ptr 1);
pass!(compact: IDirectSound_Vtbl, Compact);
pass!(get_speaker_config: IDirectSound_Vtbl, GetSpeakerConfig, ptr 1);
pass!(set_speaker_config: IDirectSound_Vtbl, SetSpeakerConfig, u 1);
pass!(ds_initialize: IDirectSound_Vtbl, Initialize, cptr 1);
pass!(verify_certification: IDirectSound8_Vtbl, VerifyCertification, ptr 1);

/// IDirectSound8, in the SDK's order (12 methods).
pub static IDIRECTSOUND8: &[Import] = &[
    ("IDirectSound8::QueryInterface", 3, ds_qi),
    ("IDirectSound8::AddRef", 1, add_ref),
    ("IDirectSound8::Release", 1, release),
    ("IDirectSound8::CreateSoundBuffer", 4, create_sound_buffer),
    ("IDirectSound8::GetCaps", 2, ds_get_caps),
    ("IDirectSound8::DuplicateSoundBuffer", 3, duplicate_sound_buffer),
    ("IDirectSound8::SetCooperativeLevel", 3, set_cooperative_level),
    ("IDirectSound8::Compact", 1, compact),
    ("IDirectSound8::GetSpeakerConfig", 2, get_speaker_config),
    ("IDirectSound8::SetSpeakerConfig", 2, set_speaker_config),
    ("IDirectSound8::Initialize", 2, ds_initialize),
    ("IDirectSound8::VerifyCertification", 2, verify_certification),
];

// ---- IDirectSoundBuffer8 ----

fn buffer_qi(c: &mut Cpu) -> u32 {
    query_interface(c, BUFFER)
}

/// Open locks by (buffer, guest address of the first part): the host's parts (address, size).
static LOCKS: Mutex<Option<HashMap<(u32, u32), [(usize, u32); 2]>>> = Mutex::new(None);

/// Lock(offset, bytes, out part 1, out size 1, out part 2, out size 2, flags): the parts in
/// the buffer's shadow (see the module comment), the host's bytes copied in.
fn buffer_lock(c: &mut Cpu) -> u32 {
    let this = arg(c, 0);
    let p = com::host(this);
    let v = unsafe { vtbl::<IDirectSoundBuffer_Vtbl>(p) };
    let (mut offset, bytes, flags) = (arg(c, 1), arg(c, 2), arg(c, 7));
    let outs = [arg(c, 3), arg(c, 4), arg(c, 5), arg(c, 6)];
    let m = mem();
    let fail = |r: u32| {
        for o in outs.into_iter().filter(|&o| o != 0) {
            m.wr32(o, 0);
        }
        r
    };
    let mut caps = DSBCAPS { dwSize: std::mem::size_of::<DSBCAPS>() as u32, ..Default::default() };
    let r = unsafe { (v.GetCaps)(p, &mut caps) };
    if r.is_err() {
        return fail(hr(r));
    }
    // the offset we lock at is where the first part goes in the shadow
    let mut host_flags = flags;
    if flags & DSBLOCK_ENTIREBUFFER != 0 {
        offset = 0;
    } else if flags & DSBLOCK_FROMWRITECURSOR != 0 {
        let r = unsafe { (v.GetCurrentPosition)(p, std::ptr::null_mut(), &mut offset) };
        if r.is_err() {
            return fail(hr(r));
        }
        host_flags &= !DSBLOCK_FROMWRITECURSOR;
    }
    let (mut h1, mut n1, mut h2, mut n2) = (std::ptr::null_mut(), 0u32, std::ptr::null_mut(), 0u32);
    let r = unsafe { (v.Lock)(p, offset, bytes, &mut h1, &mut n1, &mut h2, &mut n2, host_flags) };
    if r.is_err() {
        return fail(hr(r));
    }
    let shadow = com::shadow(this, 0, caps.dwBufferBytes);
    let wrapped = !h2.is_null() && n2 != 0;
    let (g1, g2) = guest_lock_parts(shadow, offset, wrapped);
    unsafe {
        std::ptr::copy_nonoverlapping(h1 as *const u8, m.slice(g1, n1 as usize).as_mut_ptr(), n1 as usize);
        if wrapped {
            std::ptr::copy_nonoverlapping(h2 as *const u8, m.slice(g2, n2 as usize).as_mut_ptr(), n2 as usize);
        }
    }
    let n2 = if wrapped { n2 } else { 0 };
    for (o, x) in outs.into_iter().zip([g1, n1, g2, n2]) {
        if o != 0 {
            m.wr32(o, x);
        }
    }
    lock(&LOCKS).get_or_insert_with(HashMap::new).insert((this, g1), [(h1 as usize, n1), (h2 as usize, n2)]);
    0
}

/// Unlock(part 1, bytes written 1, part 2, bytes written 2): the game's bytes to the host.
fn buffer_unlock(c: &mut Cpu) -> u32 {
    let this = arg(c, 0);
    let p = com::host(this);
    let (g1, w1, g2, w2) = (arg(c, 1), arg(c, 2), arg(c, 3), arg(c, 4));
    let Some([(h1, n1), (h2, n2)]) = lock(&LOCKS).as_mut().and_then(|t| t.remove(&(this, g1))) else {
        log!("warn: dsound: Unlock({g1:#x}) of no lock");
        return DSERR_INVALIDPARAM;
    };
    let (w1, w2) = (w1.min(n1), if g2 != 0 { w2.min(n2) } else { 0 });
    let m = mem();
    unsafe {
        std::ptr::copy_nonoverlapping(m.slice(g1, w1 as usize).as_ptr(), h1 as *mut u8, w1 as usize);
        if w2 != 0 {
            std::ptr::copy_nonoverlapping(m.slice(g2, w2 as usize).as_ptr(), h2 as *mut u8, w2 as usize);
        }
        hr((vtbl::<IDirectSoundBuffer_Vtbl>(p).Unlock)(p, h1 as *const c_void, w1, h2 as *const c_void, w2))
    }
}

/// SetFormat(format): the primary buffer's output format (logged).
fn set_format(c: &mut Cpu) -> u32 {
    let p = com::host(arg(c, 0));
    let wfx: WAVEFORMATEX = read_guest(mem(), arg(c, 1));
    let r = unsafe { (vtbl::<IDirectSoundBuffer_Vtbl>(p).SetFormat)(p, cptr(c, 1)) };
    let (rate, bits, channels) = (wfx.nSamplesPerSec, wfx.wBitsPerSample, wfx.nChannels);
    log!("dsound: SetFormat({rate} Hz, {bits} bits, {channels} channels) -> {:#x}", r.0);
    hr(r)
}

pass!(buffer_get_caps: IDirectSoundBuffer_Vtbl, GetCaps, ptr 1);
pass!(get_current_position: IDirectSoundBuffer_Vtbl, GetCurrentPosition, ptr 1, ptr 2);
pass!(get_format: IDirectSoundBuffer_Vtbl, GetFormat, ptr 1, u 2, ptr 3);
pass!(get_volume: IDirectSoundBuffer_Vtbl, GetVolume, ptr 1);
pass!(get_pan: IDirectSoundBuffer_Vtbl, GetPan, ptr 1);
pass!(get_frequency: IDirectSoundBuffer_Vtbl, GetFrequency, ptr 1);
pass!(get_status: IDirectSoundBuffer_Vtbl, GetStatus, ptr 1);
pass!(play: IDirectSoundBuffer_Vtbl, Play, u 1, u 2, u 3);
pass!(set_current_position: IDirectSoundBuffer_Vtbl, SetCurrentPosition, u 1);
pass!(set_volume: IDirectSoundBuffer_Vtbl, SetVolume, i 1);
pass!(set_pan: IDirectSoundBuffer_Vtbl, SetPan, i 1);
pass!(set_frequency: IDirectSoundBuffer_Vtbl, SetFrequency, u 1);
pass!(stop: IDirectSoundBuffer_Vtbl, Stop);
pass!(restore: IDirectSoundBuffer_Vtbl, Restore);

/// IDirectSoundBuffer8 (also for IDirectSoundBuffer, its first 21 methods), in the SDK's order.
pub static BUFFER: &[Import] = &[
    ("IDirectSoundBuffer8::QueryInterface", 3, buffer_qi),
    ("IDirectSoundBuffer8::AddRef", 1, add_ref),
    ("IDirectSoundBuffer8::Release", 1, release),
    ("IDirectSoundBuffer8::GetCaps", 2, buffer_get_caps),
    ("IDirectSoundBuffer8::GetCurrentPosition", 3, get_current_position),
    ("IDirectSoundBuffer8::GetFormat", 4, get_format),
    ("IDirectSoundBuffer8::GetVolume", 2, get_volume),
    ("IDirectSoundBuffer8::GetPan", 2, get_pan),
    ("IDirectSoundBuffer8::GetFrequency", 2, get_frequency),
    ("IDirectSoundBuffer8::GetStatus", 2, get_status),
    ("IDirectSoundBuffer8::Initialize", 3, NI),
    ("IDirectSoundBuffer8::Lock", 8, buffer_lock),
    ("IDirectSoundBuffer8::Play", 4, play),
    ("IDirectSoundBuffer8::SetCurrentPosition", 2, set_current_position),
    ("IDirectSoundBuffer8::SetFormat", 2, set_format),
    ("IDirectSoundBuffer8::SetVolume", 2, set_volume),
    ("IDirectSoundBuffer8::SetPan", 2, set_pan),
    ("IDirectSoundBuffer8::SetFrequency", 2, set_frequency),
    ("IDirectSoundBuffer8::Stop", 1, stop),
    ("IDirectSoundBuffer8::Unlock", 5, buffer_unlock),
    ("IDirectSoundBuffer8::Restore", 1, restore),
    ("IDirectSoundBuffer8::SetFX", 4, NI),
    ("IDirectSoundBuffer8::AcquireResources", 4, NI),
    ("IDirectSoundBuffer8::GetObjectInPath", 5, NI),
];

// ---- IDirectSound3DBuffer ----

fn buffer_3d_qi(c: &mut Cpu) -> u32 {
    query_interface(c, BUFFER_3D)
}

type B3 = IDirectSound3DBuffer_Vtbl;
pass!(b3_get_all: B3, GetAllParameters, ptr 1);
pass!(b3_get_cone_angles: B3, GetConeAngles, ptr 1, ptr 2);
pass!(b3_get_cone_orientation: B3, GetConeOrientation, ptr 1);
pass!(b3_get_cone_outside_volume: B3, GetConeOutsideVolume, ptr 1);
pass!(b3_get_max_distance: B3, GetMaxDistance, ptr 1);
pass!(b3_get_min_distance: B3, GetMinDistance, ptr 1);
pass!(b3_get_mode: B3, GetMode, ptr 1);
pass!(b3_get_position: B3, GetPosition, ptr 1);
pass!(b3_get_velocity: B3, GetVelocity, ptr 1);
pass!(b3_set_all: B3, SetAllParameters, cptr 1, u 2);
pass!(b3_set_cone_angles: B3, SetConeAngles, u 1, u 2, u 3);
pass!(b3_set_cone_orientation: B3, SetConeOrientation, f 1, f 2, f 3, u 4);
pass!(b3_set_cone_outside_volume: B3, SetConeOutsideVolume, i 1, u 2);
pass!(b3_set_max_distance: B3, SetMaxDistance, f 1, u 2);
pass!(b3_set_min_distance: B3, SetMinDistance, f 1, u 2);
pass!(b3_set_mode: B3, SetMode, u 1, u 2);
pass!(b3_set_position: B3, SetPosition, f 1, f 2, f 3, u 4);
pass!(b3_set_velocity: B3, SetVelocity, f 1, f 2, f 3, u 4);

/// IDirectSound3DBuffer, in the SDK's order (21 methods).
pub static BUFFER_3D: &[Import] = &[
    ("IDirectSound3DBuffer::QueryInterface", 3, buffer_3d_qi),
    ("IDirectSound3DBuffer::AddRef", 1, add_ref),
    ("IDirectSound3DBuffer::Release", 1, release),
    ("IDirectSound3DBuffer::GetAllParameters", 2, b3_get_all),
    ("IDirectSound3DBuffer::GetConeAngles", 3, b3_get_cone_angles),
    ("IDirectSound3DBuffer::GetConeOrientation", 2, b3_get_cone_orientation),
    ("IDirectSound3DBuffer::GetConeOutsideVolume", 2, b3_get_cone_outside_volume),
    ("IDirectSound3DBuffer::GetMaxDistance", 2, b3_get_max_distance),
    ("IDirectSound3DBuffer::GetMinDistance", 2, b3_get_min_distance),
    ("IDirectSound3DBuffer::GetMode", 2, b3_get_mode),
    ("IDirectSound3DBuffer::GetPosition", 2, b3_get_position),
    ("IDirectSound3DBuffer::GetVelocity", 2, b3_get_velocity),
    ("IDirectSound3DBuffer::SetAllParameters", 3, b3_set_all),
    ("IDirectSound3DBuffer::SetConeAngles", 4, b3_set_cone_angles),
    ("IDirectSound3DBuffer::SetConeOrientation", 5, b3_set_cone_orientation),
    ("IDirectSound3DBuffer::SetConeOutsideVolume", 3, b3_set_cone_outside_volume),
    ("IDirectSound3DBuffer::SetMaxDistance", 3, b3_set_max_distance),
    ("IDirectSound3DBuffer::SetMinDistance", 3, b3_set_min_distance),
    ("IDirectSound3DBuffer::SetMode", 3, b3_set_mode),
    ("IDirectSound3DBuffer::SetPosition", 5, b3_set_position),
    ("IDirectSound3DBuffer::SetVelocity", 5, b3_set_velocity),
];

// ---- IDirectSound3DListener ----

fn listener_qi(c: &mut Cpu) -> u32 {
    query_interface(c, LISTENER)
}

type L3 = IDirectSound3DListener_Vtbl;
pass!(l_get_all: L3, GetAllParameters, ptr 1);
pass!(l_get_distance_factor: L3, GetDistanceFactor, ptr 1);
pass!(l_get_doppler_factor: L3, GetDopplerFactor, ptr 1);
pass!(l_get_orientation: L3, GetOrientation, ptr 1, ptr 2);
pass!(l_get_position: L3, GetPosition, ptr 1);
pass!(l_get_rolloff_factor: L3, GetRolloffFactor, ptr 1);
pass!(l_get_velocity: L3, GetVelocity, ptr 1);
pass!(l_set_all: L3, SetAllParameters, cptr 1, u 2);
pass!(l_set_distance_factor: L3, SetDistanceFactor, f 1, u 2);
pass!(l_set_doppler_factor: L3, SetDopplerFactor, f 1, u 2);
pass!(l_set_orientation: L3, SetOrientation, f 1, f 2, f 3, f 4, f 5, f 6, u 7);
pass!(l_set_position: L3, SetPosition, f 1, f 2, f 3, u 4);
pass!(l_set_rolloff_factor: L3, SetRolloffFactor, f 1, u 2);
pass!(l_set_velocity: L3, SetVelocity, f 1, f 2, f 3, u 4);
pass!(l_commit: L3, CommitDeferredSettings);

/// IDirectSound3DListener, in the SDK's order (18 methods).
pub static LISTENER: &[Import] = &[
    ("IDirectSound3DListener::QueryInterface", 3, listener_qi),
    ("IDirectSound3DListener::AddRef", 1, add_ref),
    ("IDirectSound3DListener::Release", 1, release),
    ("IDirectSound3DListener::GetAllParameters", 2, l_get_all),
    ("IDirectSound3DListener::GetDistanceFactor", 2, l_get_distance_factor),
    ("IDirectSound3DListener::GetDopplerFactor", 2, l_get_doppler_factor),
    ("IDirectSound3DListener::GetOrientation", 3, l_get_orientation),
    ("IDirectSound3DListener::GetPosition", 2, l_get_position),
    ("IDirectSound3DListener::GetRolloffFactor", 2, l_get_rolloff_factor),
    ("IDirectSound3DListener::GetVelocity", 2, l_get_velocity),
    ("IDirectSound3DListener::SetAllParameters", 3, l_set_all),
    ("IDirectSound3DListener::SetDistanceFactor", 3, l_set_distance_factor),
    ("IDirectSound3DListener::SetDopplerFactor", 3, l_set_doppler_factor),
    ("IDirectSound3DListener::SetOrientation", 8, l_set_orientation),
    ("IDirectSound3DListener::SetPosition", 5, l_set_position),
    ("IDirectSound3DListener::SetRolloffFactor", 3, l_set_rolloff_factor),
    ("IDirectSound3DListener::SetVelocity", 5, l_set_velocity),
    ("IDirectSound3DListener::CommitDeferredSettings", 1, l_commit),
];

// ---- IDirectSoundNotify ----

fn notify_qi(c: &mut Cpu) -> u32 {
    query_interface(c, NOTIFY)
}

/// SetNotificationPositions(count, positions): the game's events as host events (`host_event`).
fn set_notification_positions(c: &mut Cpu) -> u32 {
    let p = com::host(arg(c, 0));
    let n = arg(c, 1);
    let v = notify_positions_from_guest(mem(), arg(c, 2), n, host_event);
    hr(unsafe { (vtbl::<IDirectSoundNotify_Vtbl>(p).SetNotificationPositions)(p, n, v.as_ptr()) })
}

/// IDirectSoundNotify, in the SDK's order (4 methods).
pub static NOTIFY: &[Import] = &[
    ("IDirectSoundNotify::QueryInterface", 3, notify_qi),
    ("IDirectSoundNotify::AddRef", 1, add_ref),
    ("IDirectSoundNotify::Release", 1, release),
    ("IDirectSoundNotify::SetNotificationPositions", 3, set_notification_positions),
];

pub const IMPORTS: &[Import] = &[("#11", 3, direct_sound_create8)];
