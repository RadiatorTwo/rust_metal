//! DirectInput 7 for the keyboard and the mouse (from rustschweine's, without its joysticks:
//! gamepads come later; for now the game finds none).
//!
//! The game makes DirectInput as DirectInputCreateEx would, through COM (di_create 0x4dded0):
//! CoCreateInstance(CLSID_DirectInput, IID_IDirectInput7W), QueryInterface for
//! IID_IDirectInput7A, Initialize(instance, 0x700). It does that three times in input_init
//! (0x4ddcc0), once per device kind, each time keeping its own IDirectInput7A:
//! - keyboard (kb_init 0x4dcba0): CreateDeviceEx(GUID_SysKeyboard, IID_IDirectInputDevice7A),
//!   SetDataFormat(c_dfDIKeyboard), SetCooperativeLevel(non-exclusive | foreground | no
//!   Windows key), SetProperty(DIPROP_BUFFERSIZE, 10 events; kb_set_buffer 0x4dcc60), Acquire.
//!   It reads the state (GetDeviceState, 256 bytes; kb_read 0x4dced0) and, for a key not down
//!   now, the buffered events since the last read (kb_key_down 0x4dcdc0, kb_buffered 0x4dcce0),
//!   so that a short press between two reads still counts;
//! - mouse (mouse_init 0x4dd010): GUID_SysMouse, c_dfDIMouse2 (20 bytes: x, y, wheel, 8
//!   buttons), exclusive | foreground, Acquire; read by state (mouse_read 0x4dd0d0);
//! - joysticks (joy_enum 0x4dd6f0, joy_init 0x4dd820): EnumDevices(DIDEVTYPE_JOYSTICK,
//!   attached only); none found is fine ("Joystick/joypad not found" in gunmetal.log).
//! Only the A interface is called; the W object it is made as is the same object here.
//!
//! Keyboard and mouse come from the window's input (`user32`): the keys by DIK code (the scan
//! code `window` gives each key, bit 7 for extended keys such as right Ctrl and Alt), the
//! mouse's movement since the last read and its buttons. Buffered data (GetDeviceData) is
//! always empty for now.

use super::ole32::{guid, guid_at};
use super::{Import, arg, free, mem};
use crate::rt::Cpu;
use std::sync::Mutex;

const DI_OK: u32 = 0;
const E_NOTIMPL: u32 = 0x8000_4001;
const E_NOINTERFACE: u32 = 0x8000_4002;
const E_INVALIDARG: u32 = 0x8007_0057;
const DIERR_DEVICENOTREG: u32 = 0x8004_0154;

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
const GUID_SYSMOUSE: [u8; 16] = guid(0x6f1d_2b60, 0xd5a0, 0x11cf, DI_D4);
const GUID_SYSKEYBOARD: [u8; 16] = guid(0x6f1d_2b61, 0xd5a0, 0x11cf, DI_D4);
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
    mem().wr32(out, d);
    DI_OK
}

/// EnumDevices(type, callback, ref, flags): the keyboard and the mouse are not enumerated (the
/// game asks only for joysticks), and there are no joysticks yet.
fn enum_devices(_: &mut Cpu) -> u32 {
    DI_OK
}

fn ok(_: &mut Cpu) -> u32 {
    DI_OK
}

fn not_impl(_: &mut Cpu) -> u32 {
    E_NOTIMPL
}

static DIRECT_INPUT7: &[Import] = &[
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
// object: vtable, references, kind (KEYBOARD or MOUSE)

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

/// GetDeviceData(object size, data, count, flags): no buffered data yet.
fn get_device_data(c: &mut Cpu) -> u32 {
    if arg(c, 3) != 0 {
        mem().wr32(arg(c, 3), 0);
    }
    DI_OK
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

/// The mouse's state (DIMOUSESTATE or DIMOUSESTATE2): movement since the last read, buttons.
fn mouse_state(out: u32, size: u32) -> u32 {
    static LAST: Mutex<Option<(i32, i32)>> = Mutex::new(None);
    let (x, y) = super::user32::mouse_pos();
    let (lx, ly) = LAST.lock().unwrap_or_else(|e| e.into_inner()).replace((x, y)).unwrap_or((x, y));
    let m = mem();
    m.slice(out, size as usize).fill(0);
    m.wr32(out, (x - lx) as u32);
    m.wr32(out + 4, (y - ly) as u32);
    for (k, vk) in [1u8, 2, 4].into_iter().enumerate() {
        if 12 + k < size as usize && super::user32::key_down(vk) {
            m.wr8(out + 12 + k as u32, 0x80);
        }
    }
    DI_OK
}

/// GetDeviceState(size, out): the keyboard or the mouse.
fn get_device_state(c: &mut Cpu) -> u32 {
    let (d, size, out) = (arg(c, 0), arg(c, 1), arg(c, 2));
    match mem().rd32(d + 8) {
        KEYBOARD => keyboard_state(out, size),
        _ => mouse_state(out, size),
    }
}

static DEVICE: &[Import] = &[
    ("IDirectInputDevice7A::QueryInterface", 3, dev_query),
    ("IDirectInputDevice7A::AddRef", 1, add_ref),
    ("IDirectInputDevice7A::Release", 1, release),
    ("IDirectInputDevice7A::GetCapabilities", 2, get_capabilities),
    ("IDirectInputDevice7A::EnumObjects", 4, ok),
    ("IDirectInputDevice7A::GetProperty", 3, not_impl),
    ("IDirectInputDevice7A::SetProperty", 3, ok),
    ("IDirectInputDevice7A::Acquire", 1, ok),
    ("IDirectInputDevice7A::Unacquire", 1, ok),
    ("IDirectInputDevice7A::GetDeviceState", 3, get_device_state),
    ("IDirectInputDevice7A::GetDeviceData", 5, get_device_data),
    ("IDirectInputDevice7A::SetDataFormat", 2, ok),
    ("IDirectInputDevice7A::SetEventNotification", 2, ok),
    ("IDirectInputDevice7A::SetCooperativeLevel", 3, ok),
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
