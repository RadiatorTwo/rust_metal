//! DirectInput 7 for the keyboard and the mouse (from rustschweine's, without joysticks: the
//! game enumerates them and goes on without; gamepads would need a joystick device of their
//! own, DIJOYSTATE with its objects and ranges, which is left for later).
//!
//! The game makes DirectInput as DirectInputCreateEx would, through COM (di_create 0x4dded0):
//! CoCreateInstance(CLSID_DirectInput, IID_IDirectInput7W), QueryInterface for
//! IID_IDirectInput7A, Initialize(instance, 0x700). It does that three times in input_init
//! (0x4ddcc0), once per device kind, each time keeping its own IDirectInput7A:
//! - keyboard (kb_init 0x4dcba0): CreateDeviceEx(GUID_SysKeyboard, IID_IDirectInputDevice7A),
//!   SetDataFormat(c_dfDIKeyboard), SetCooperativeLevel(non-exclusive | foreground | no
//!   Windows key), SetProperty(DIPROP_BUFFERSIZE, 10 events; kb_set_buffer 0x4dcc60), Acquire.
//!   It reads the state (GetDeviceState, 256 bytes; kb_read 0x4dced0, kb_key_down 0x4dcdc0).
//!   For a key not down now kb_key_down takes the buffered events since the last time
//!   (kb_buffered 0x4dcce0: GetDeviceData of 16-byte DIDEVICEOBJECTDATA; DI_BUFFEROVERFLOW
//!   counts as success, other errors re-Acquire) and marks every key pressed in them down in
//!   its state copy, so that a tap between two reads still counts;
//! - mouse (mouse_init 0x4dd010): GUID_SysMouse, c_dfDIMouse2 (20 bytes: x, y, wheel, 8
//!   buttons), exclusive | foreground, Acquire. game_start unacquires it again (0x45a0a0), as
//!   do a few other places; mouse_read (0x4dd0d0) acquires it when GetDeviceState says it is
//!   not acquired. Its movement drives the game's own cursor in the menus and the aim in
//!   missions (mouse_update 0x4477d0: x and y times a sensitivity, buttons 0-5, the wheel as
//!   two more buttons); the key binding menu (bind_capture 0x4ed4f0) takes buttons 0-6 and
//!   the wheel as well;
//! - joysticks (joy_enum 0x4dd6f0, joy_init 0x4dd820): EnumDevices(DIDEVTYPE_JOYSTICK,
//!   attached only); none found is fine ("Joystick/joypad not found" in gunmetal.log).
//! Only the A interface is called; the W object it is made as is the same object here.
//!
//! Keyboard and mouse come from the window's input (`user32`): the keys by DIK code (the scan
//! code `window` gives each key, bit 7 for extended keys such as right Ctrl and Alt); each
//! change of a key is also an event in every acquired keyboard's buffer (`key_event`). The
//! mouse's x and y are the raw movement since the last read (`user32::take_motion`: raw
//! input, so it does not stop at the edge of the window or the screen), the wheel 120 per
//! notch. While an exclusive mouse is acquired the window hides the cursor and keeps it inside
//! (`window::catch_mouse`), as DirectInput did.

use super::ole32::{guid, guid_at};
use super::{Import, arg, free, mem};
use crate::rt::Cpu;
use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, MutexGuard};

const DI_OK: u32 = 0;
/// Acquire of an acquired device, Unacquire of one that is not (DI_NOEFFECT)
const S_FALSE: u32 = 1;
pub const DI_BUFFEROVERFLOW: u32 = 1;
const E_NOTIMPL: u32 = 0x8000_4001;
const E_NOINTERFACE: u32 = 0x8000_4002;
const E_INVALIDARG: u32 = 0x8007_0057;
const DIERR_DEVICENOTREG: u32 = 0x8004_0154;
pub const DIERR_NOTACQUIRED: u32 = 0x8007_000c;
pub const DIERR_NOTBUFFERED: u32 = 0x8004_0207;

/// SetProperty/GetProperty: MAKEDIPROP(1), the "GUID pointer" is the number
pub const DIPROP_BUFFERSIZE: u32 = 1;
pub const DIGDD_PEEK: u32 = 1;
pub const DISCL_EXCLUSIVE: u32 = 1;
/// DIDEVICEOBJECTDATA of DirectInput 7: dwOfs, dwData, dwTimeStamp, dwSequence (DirectInput 8
/// adds uAppData; a larger size is accepted and the rest zeroed)
pub const OBJECT_DATA_SIZE: u32 = 16;

const DIDEVTYPE_KEYBOARD: u32 = 3;
const DIDEVTYPE_MOUSE: u32 = 2;
const DIDC_ATTACHED: u32 = 1;

const DI_D4: [u8; 8] = [0xbf, 0xc7, 0x44, 0x45, 0x53, 0x54, 0x00, 0x00];
const DI7_D4: [u8; 8] = [0x8e, 0x9d, 0x00, 0xc0, 0x4f, 0x68, 0x44, 0xae];
pub const CLSID_DIRECTINPUT: [u8; 16] = guid(0x25e6_09e0, 0xb259, 0x11cf, DI_D4);
const IID_IUNKNOWN: [u8; 16] = guid(0, 0, 0, [0xc0, 0, 0, 0, 0, 0, 0, 0x46]);
/// IDirectInputA/W, IDirectInput2A/W, IDirectInput7A/W: one object answers them all.
const IID_DIRECTINPUT: [[u8; 16]; 7] = [
    IID_IUNKNOWN,
    guid(0x8952_1360, 0xaa8a, 0x11cf, DI_D4),
    guid(0x8952_1361, 0xaa8a, 0x11cf, DI_D4),
    guid(0x5944_e662, 0xc92e, 0x11cf, DI_D4),
    guid(0x5944_e663, 0xc92e, 0x11cf, DI_D4),
    guid(0x9a4c_b684, 0x236d, 0x11d3, DI7_D4),
    guid(0x9a4c_b685, 0x236d, 0x11d3, DI7_D4),
];
/// IDirectInputDeviceA/W, IDirectInputDevice2A/W, IDirectInputDevice7A/W.
const IID_DEVICE: [[u8; 16]; 7] = [
    IID_IUNKNOWN,
    guid(0x5944_e680, 0xc92e, 0x11cf, DI_D4),
    guid(0x5944_e681, 0xc92e, 0x11cf, DI_D4),
    guid(0x5944_e682, 0xc92e, 0x11cf, DI_D4),
    guid(0x5944_e683, 0xc92e, 0x11cf, DI_D4),
    guid(0x57d7_c6bc, 0x2356, 0x11d3, DI7_D4),
    guid(0x57d7_c6bd, 0x2356, 0x11d3, DI7_D4),
];
pub const GUID_SYSMOUSE: [u8; 16] = guid(0x6f1d_2b60, 0xd5a0, 0x11cf, DI_D4);
pub const GUID_SYSKEYBOARD: [u8; 16] = guid(0x6f1d_2b61, 0xd5a0, 0x11cf, DI_D4);
/// the device kind at +8
const KEYBOARD: u32 = 0x100;
const MOUSE: u32 = 0x101;

/// The IIDs an IDirectInput object answers.
pub fn is_direct_input_iid(iid: &[u8; 16]) -> bool {
    IID_DIRECTINPUT.contains(iid)
}

/// CoCreateInstance(CLSID_DirectInput) for interface `iid`: a new, not yet initialised object.
pub fn create(iid: u32, out: u32) -> u32 {
    if !is_direct_input_iid(&guid_at(iid)) {
        mem().wr32(out, 0);
        return E_NOINTERFACE;
    }
    let o = super::com_new(DIRECT_INPUT7, 4);
    mem().wr32(o + 4, 1);
    mem().wr32(out, o);
    DI_OK
}

// ---- buffered data ----------------------------------------------------------------------------

/// One buffered event (DIDEVICEOBJECTDATA): for a keyboard `ofs` is the DIK code and `data`
/// 0x80 for a press, 0 for a release.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Event {
    pub ofs: u32,
    pub data: u32,
    /// ms, as GetTickCount
    pub time: u32,
    pub seq: u32,
}

/// A device's buffer of DIPROP_BUFFERSIZE events. As DirectInput (and Wine), a full buffer
/// keeps its events and loses the new ones, and says so once (DI_BUFFEROVERFLOW).
#[derive(Debug, Default)]
pub struct Buffer {
    size: usize,
    events: VecDeque<Event>,
    overflow: bool,
}

impl Buffer {
    pub fn new(size: usize) -> Self {
        Buffer { size, ..Default::default() }
    }

    pub fn size(&self) -> usize {
        self.size
    }

    pub fn push(&mut self, e: Event) {
        if self.size == 0 {
            return;
        }
        if self.events.len() >= self.size {
            self.overflow = true;
            return;
        }
        self.events.push_back(e);
    }

    /// Up to `n` events, oldest first, and whether the buffer overflowed since it was last
    /// read; both stay (`peek`) or are taken.
    pub fn take(&mut self, n: usize, peek: bool) -> (Vec<Event>, bool) {
        let n = n.min(self.events.len());
        let overflow = self.overflow;
        if peek {
            return (self.events.iter().take(n).copied().collect(), overflow);
        }
        self.overflow = false;
        (self.events.drain(..n).collect(), overflow)
    }

    pub fn clear(&mut self) {
        self.events.clear();
        self.overflow = false;
    }
}

/// What a device object is besides its guest block.
struct Dev {
    kind: u32,
    acquired: bool,
    exclusive: bool,
    buffer: Buffer,
}

/// The devices by guest address.
static DEVICES: Mutex<BTreeMap<u32, Dev>> = Mutex::new(BTreeMap::new());

fn devices() -> MutexGuard<'static, BTreeMap<u32, Dev>> {
    DEVICES.lock().unwrap_or_else(|e| e.into_inner())
}

/// A key changed (DIK code `dik`): an event in every acquired keyboard's buffer.
pub fn key_event(dik: u8, down: bool) {
    static SEQ: AtomicU32 = AtomicU32::new(1);
    let e = Event {
        ofs: dik as u32,
        data: if down { 0x80 } else { 0 },
        time: super::kernel32::ms_since_start(),
        seq: SEQ.fetch_add(1, Ordering::Relaxed),
    };
    for d in devices().values_mut().filter(|d| d.kind == KEYBOARD && d.acquired) {
        d.buffer.push(e);
    }
}

// ---- IDirectInput7A ---------------------------------------------------------------------------
// object: vtable, references

fn di_query(c: &mut Cpu) -> u32 {
    let (this, iid, out) = (arg(c, 0), guid_at(arg(c, 1)), arg(c, 2));
    if !is_direct_input_iid(&iid) {
        mem().wr32(out, 0);
        return E_NOINTERFACE;
    }
    add_ref(c);
    mem().wr32(out, this);
    DI_OK
}

/// AddRef and Release of both interfaces: the count is at +4.
fn add_ref(c: &mut Cpu) -> u32 {
    let o = arg(c, 0);
    let n = mem().rd32(o + 4) + 1;
    mem().wr32(o + 4, n);
    n
}

fn release(c: &mut Cpu) -> u32 {
    let o = arg(c, 0);
    let n = mem().rd32(o + 4).saturating_sub(1);
    mem().wr32(o + 4, n);
    if n == 0 {
        free(o);
    }
    n
}

fn create_device(c: &mut Cpu) -> u32 {
    let (g, out) = (guid_at(arg(c, 1)), arg(c, 2));
    new_device(g, out)
}

/// CreateDeviceEx(guid, iid, out, outer)
fn create_device_ex(c: &mut Cpu) -> u32 {
    let (g, iid, out) = (guid_at(arg(c, 1)), guid_at(arg(c, 2)), arg(c, 3));
    if !IID_DEVICE.contains(&iid) {
        mem().wr32(out, 0);
        return E_NOINTERFACE;
    }
    new_device(g, out)
}

fn new_device(g: [u8; 16], out: u32) -> u32 {
    let kind = match g {
        GUID_SYSKEYBOARD => KEYBOARD,
        GUID_SYSMOUSE => MOUSE,
        _ => {
            mem().wr32(out, 0);
            return DIERR_DEVICENOTREG;
        }
    };
    let d = super::com_new(DEVICE, 8);
    mem().wr32(d + 4, 1);
    mem().wr32(d + 8, kind);
    devices().insert(d, Dev { kind, acquired: false, exclusive: false, buffer: Buffer::default() });
    mem().wr32(out, d);
    DI_OK
}

/// EnumDevices(type, callback, ref, flags): the keyboard and the mouse are not enumerated (the
/// game asks only for joysticks), and there are no joysticks.
fn enum_devices(_: &mut Cpu) -> u32 {
    DI_OK
}

fn ok(_: &mut Cpu) -> u32 {
    DI_OK
}

fn not_impl(_: &mut Cpu) -> u32 {
    E_NOTIMPL
}

pub static DIRECT_INPUT7: &[Import] = &[
    ("IDirectInput7A::QueryInterface", 3, di_query),
    ("IDirectInput7A::AddRef", 1, add_ref),
    ("IDirectInput7A::Release", 1, release),
    ("IDirectInput7A::CreateDevice", 4, create_device),
    ("IDirectInput7A::EnumDevices", 5, enum_devices),
    ("IDirectInput7A::GetDeviceStatus", 2, ok),
    ("IDirectInput7A::RunControlPanel", 3, ok),
    ("IDirectInput7A::Initialize", 3, ok),
    ("IDirectInput7A::FindDevice", 4, not_impl),
    ("IDirectInput7A::CreateDeviceEx", 5, create_device_ex),
];

// ---- IDirectInputDevice7A ---------------------------------------------------------------------
// object: vtable, references, kind (KEYBOARD or MOUSE); the rest in DEVICES

fn dev_query(c: &mut Cpu) -> u32 {
    let (this, iid, out) = (arg(c, 0), guid_at(arg(c, 1)), arg(c, 2));
    if !IID_DEVICE.contains(&iid) {
        mem().wr32(out, 0);
        return E_NOINTERFACE;
    }
    add_ref(c);
    mem().wr32(out, this);
    DI_OK
}

/// Release: the last one unacquires (the mouse frees the cursor) and forgets the device.
fn dev_release(c: &mut Cpu) -> u32 {
    let d = arg(c, 0);
    if mem().rd32(d + 4) == 1 {
        unacquire_device(d);
        devices().remove(&d);
    }
    release(c)
}

/// Acquire: a fresh buffer; the mouse's movement counts from now, an exclusive mouse catches
/// the cursor.
fn acquire(c: &mut Cpu) -> u32 {
    let d = arg(c, 0);
    let (kind, exclusive) = {
        let mut ds = devices();
        let Some(dev) = ds.get_mut(&d) else { return DI_OK };
        if dev.acquired {
            return S_FALSE;
        }
        dev.acquired = true;
        dev.buffer.clear();
        (dev.kind, dev.exclusive)
    };
    if kind == MOUSE {
        super::user32::take_motion();
        if exclusive {
            super::window::catch_mouse(true);
        }
    }
    DI_OK
}

fn unacquire(c: &mut Cpu) -> u32 {
    unacquire_device(arg(c, 0))
}

fn unacquire_device(d: u32) -> u32 {
    let (kind, exclusive) = {
        let mut ds = devices();
        let Some(dev) = ds.get_mut(&d).filter(|dev| dev.acquired) else { return S_FALSE };
        dev.acquired = false;
        (dev.kind, dev.exclusive)
    };
    if kind == MOUSE && exclusive {
        super::window::catch_mouse(false);
    }
    DI_OK
}

/// SetCooperativeLevel(hwnd, flags): exclusive or not.
fn set_cooperative_level(c: &mut Cpu) -> u32 {
    if let Some(dev) = devices().get_mut(&arg(c, 0)) {
        dev.exclusive = arg(c, 2) & DISCL_EXCLUSIVE != 0;
    }
    DI_OK
}

/// SetProperty(property, DIPROPHEADER): DIPROP_BUFFERSIZE (DIPROPDWORD, the value after the
/// 16-byte header); the others are taken and ignored.
fn set_property(c: &mut Cpu) -> u32 {
    let (d, prop, p) = (arg(c, 0), arg(c, 1), arg(c, 2));
    if prop == DIPROP_BUFFERSIZE {
        let n = mem().rd32(p + 16);
        if let Some(dev) = devices().get_mut(&d) {
            dev.buffer = Buffer::new(n as usize);
        }
    }
    DI_OK
}

/// GetProperty(property, DIPROPHEADER): DIPROP_BUFFERSIZE.
fn get_property(c: &mut Cpu) -> u32 {
    let (d, prop, p) = (arg(c, 0), arg(c, 1), arg(c, 2));
    if prop != DIPROP_BUFFERSIZE {
        return E_NOTIMPL;
    }
    let Some(n) = devices().get(&d).map(|dev| dev.buffer.size()) else { return E_INVALIDARG };
    mem().wr32(p + 16, n as u32);
    DI_OK
}

/// GetDeviceData(object size, data, count in/out, flags): up to `count` buffered events into
/// `data`, taken unless DIGDD_PEEK; with data 0 they are only counted, or dropped (count
/// INFINITE and no flags flushes the buffer).
fn get_device_data(c: &mut Cpu) -> u32 {
    let (d, size, data, count, flags) = (arg(c, 0), arg(c, 1), arg(c, 2), arg(c, 3), arg(c, 4));
    let m = mem();
    if count == 0 || size < OBJECT_DATA_SIZE {
        return E_INVALIDARG;
    }
    let mut ds = devices();
    let Some(dev) = ds.get_mut(&d) else { return E_INVALIDARG };
    if !dev.acquired {
        return DIERR_NOTACQUIRED;
    }
    if dev.buffer.size() == 0 {
        return DIERR_NOTBUFFERED;
    }
    let (events, overflow) = dev.buffer.take(m.rd32(count) as usize, flags & DIGDD_PEEK != 0);
    drop(ds);
    if data != 0 {
        for (k, e) in events.iter().enumerate() {
            let p = data + k as u32 * size;
            m.slice(p, size as usize).fill(0);
            for (n, v) in [e.ofs, e.data, e.time, e.seq].into_iter().enumerate() {
                m.wr32(p + 4 * n as u32, v);
            }
        }
    }
    m.wr32(count, events.len() as u32);
    if overflow { DI_BUFFEROVERFLOW } else { DI_OK }
}

/// GetCapabilities(DIDEVCAPS): attached; the keyboard with 256 buttons (keys), the mouse
/// with 3 axes and 8 buttons.
fn get_capabilities(c: &mut Cpu) -> u32 {
    let (d, p) = (arg(c, 0), arg(c, 1));
    let m = mem();
    let size = m.rd32(p);
    if size < 24 {
        return E_INVALIDARG;
    }
    m.slice(p + 4, size as usize - 4).fill(0);
    let (ty, axes, buttons) = if m.rd32(d + 8) == KEYBOARD { (DIDEVTYPE_KEYBOARD, 0, 256) } else { (DIDEVTYPE_MOUSE, 3, 8) };
    for (k, v) in [DIDC_ATTACHED, ty, axes, buttons, 0].into_iter().enumerate() {
        m.wr32(p + 4 + 4 * k as u32, v);
    }
    DI_OK
}

/// The keyboard's state: 256 bytes by DIK code (the window's scan codes, bit 7 for extended
/// keys), 0x80 when down.
pub fn keyboard_state(out: u32, size: u32) -> u32 {
    let mut s = [0u8; 256];
    for k in 0..=255u8 {
        if super::user32::dik_down(k) {
            s[k as usize] = 0x80;
        }
    }
    let n = (size as usize).min(256);
    mem().slice(out, n).copy_from_slice(&s[..n]);
    DI_OK
}

/// The mouse's state (DIMOUSESTATE, 16 bytes, or DIMOUSESTATE2, 20): movement and wheel since
/// the last read, buttons (left, right, middle, back, forward).
pub fn mouse_state(out: u32, size: u32) -> u32 {
    let (x, y, z) = super::user32::take_motion();
    let m = mem();
    m.slice(out, size as usize).fill(0);
    for (k, v) in [x, y, z].into_iter().enumerate() {
        if 4 * k + 4 <= size as usize {
            m.wr32(out + 4 * k as u32, v as u32);
        }
    }
    for (k, vk) in [1u8, 2, 4, 5, 6].into_iter().enumerate() {
        if 12 + k < size as usize && super::user32::key_down(vk) {
            m.wr8(out + 12 + k as u32, 0x80);
        }
    }
    DI_OK
}

/// GetDeviceState(size, out): the keyboard or the mouse, once acquired.
fn get_device_state(c: &mut Cpu) -> u32 {
    let (d, size, out) = (arg(c, 0), arg(c, 1), arg(c, 2));
    if devices().get(&d).is_some_and(|dev| !dev.acquired) {
        return DIERR_NOTACQUIRED;
    }
    match mem().rd32(d + 8) {
        KEYBOARD => keyboard_state(out, size),
        _ => mouse_state(out, size),
    }
}

pub static DEVICE: &[Import] = &[
    ("IDirectInputDevice7A::QueryInterface", 3, dev_query),
    ("IDirectInputDevice7A::AddRef", 1, add_ref),
    ("IDirectInputDevice7A::Release", 1, dev_release),
    ("IDirectInputDevice7A::GetCapabilities", 2, get_capabilities),
    ("IDirectInputDevice7A::EnumObjects", 4, ok),
    ("IDirectInputDevice7A::GetProperty", 3, get_property),
    ("IDirectInputDevice7A::SetProperty", 3, set_property),
    ("IDirectInputDevice7A::Acquire", 1, acquire),
    ("IDirectInputDevice7A::Unacquire", 1, unacquire),
    ("IDirectInputDevice7A::GetDeviceState", 3, get_device_state),
    ("IDirectInputDevice7A::GetDeviceData", 5, get_device_data),
    ("IDirectInputDevice7A::SetDataFormat", 2, ok),
    ("IDirectInputDevice7A::SetEventNotification", 2, ok),
    ("IDirectInputDevice7A::SetCooperativeLevel", 3, set_cooperative_level),
    ("IDirectInputDevice7A::GetObjectInfo", 4, not_impl),
    ("IDirectInputDevice7A::GetDeviceInfo", 2, not_impl),
    ("IDirectInputDevice7A::RunControlPanel", 3, ok),
    ("IDirectInputDevice7A::Initialize", 4, ok),
    ("IDirectInputDevice7A::CreateEffect", 5, not_impl),
    ("IDirectInputDevice7A::EnumEffects", 4, ok),
    ("IDirectInputDevice7A::GetEffectInfo", 3, not_impl),
    ("IDirectInputDevice7A::GetForceFeedbackState", 2, not_impl),
    ("IDirectInputDevice7A::SendForceFeedbackCommand", 2, not_impl),
    ("IDirectInputDevice7A::EnumCreatedEffectObjects", 4, ok),
    ("IDirectInputDevice7A::Escape", 2, not_impl),
    ("IDirectInputDevice7A::Poll", 1, ok),
    ("IDirectInputDevice7A::SendDeviceData", 5, not_impl),
    ("IDirectInputDevice7A::EnumEffectsInFile", 5, not_impl),
    ("IDirectInputDevice7A::WriteEffectToFile", 5, not_impl),
];
