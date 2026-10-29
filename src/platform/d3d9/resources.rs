//! Direct3D 9 resources, as far as the game needs them before its first Present: textures,
//! vertex and index buffers, queries. The game loads its first textures (DXT, through its own
//! loader 0x479e80) and fills buffers right after CreateDevice; a resource it cannot create
//! ends the game (0x47ab30).
//!
//! A lock gives the game a guest buffer (`com::shadow`, kept per object and level; a buffer's
//! covers the whole buffer and a lock's range is at its offset) with the host's pitch; the
//! host's resource stays locked until the game unlocks, then the guest bytes are copied to it.
//! Locks may nest (D3D9 counts them): each object and level keeps a stack of open locks, and
//! each Unlock copies the range of the latest. The host's contents are copied in at the lock unless the lock
//! discards them (D3DLOCK_DISCARD), and not copied back for D3DLOCK_READONLY.
//!
//! Not here yet (Task 6): render targets and other surfaces of the device, cube and volume textures, vertex declarations, shaders,
//! state blocks; private data and GetDC are logged at their first call.

use super::{add_ref, cp, gp, hr, query_interface, release, vtbl};
use crate::platform::{Import, arg, com, lock, log, mem, method_not_implemented as NI};
use crate::rt::Cpu;
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::Mutex;
use windows::Win32::Graphics::Direct3D9::*;
use windows::core::Interface;

pub const D3DLOCK_READONLY: u32 = 0x10;
pub const D3DLOCK_DISCARD: u32 = 0x2000;

/// Whether a lock with `flags` copies the host's contents into the guest buffer: not when
/// they are discarded.
pub fn lock_copy_in(flags: u32) -> bool {
    flags & D3DLOCK_DISCARD == 0
}

/// A lock in progress: the host's locked bytes, how many, and the lock's flags.
struct Locked {
    host: usize,
    guest: u32,
    size: usize,
    flags: u32,
}

/// Open locks by (guest object, level), the latest last.
static LOCKS: Mutex<Option<HashMap<(u32, u32), Vec<Locked>>>> = Mutex::new(None);

/// Start a lock of (`this`, `level`): `size` host bytes at `host` are the guest's at `guest`
/// (copied there unless the lock discards them).
pub fn begin_lock(this: u32, level: u32, guest: u32, host: *mut c_void, size: usize, flags: u32) {
    if lock_copy_in(flags) && size != 0 {
        mem().slice(guest, size).copy_from_slice(unsafe { std::slice::from_raw_parts(host as *const u8, size) });
    }
    let mut locks = lock(&LOCKS);
    locks.get_or_insert_with(HashMap::new).entry((this, level)).or_default().push(Locked { host: host as usize, guest, size, flags });
}

/// End the latest lock of (`this`, `level`): its guest bytes to the host (unless read-only).
/// False if there was none.
pub fn end_lock(this: u32, level: u32) -> bool {
    let Some(l) = lock(&LOCKS).as_mut().and_then(|m| m.get_mut(&(this, level))).and_then(|v| v.pop()) else {
        return false;
    };
    if l.size == 0 {
        return true;
    }
    if l.flags & D3DLOCK_READONLY == 0 {
        unsafe { std::slice::from_raw_parts_mut(l.host as *mut u8, l.size) }.copy_from_slice(mem().slice(l.guest, l.size));
    }
    true
}

fn res(c: &Cpu) -> (*mut c_void, &'static IDirect3DResource9_Vtbl) {
    let p = com::host(arg(c, 0));
    (p, unsafe { vtbl(p) })
}

/// IDirect3DResource9::GetDevice: the game's device object.
fn get_device(c: &mut Cpu) -> u32 {
    let (p, v) = res(c);
    let mut d: *mut c_void = std::ptr::null_mut();
    let r = unsafe { (v.GetDevice)(p, &mut d) };
    mem().wr32(arg(c, 1), if r.is_ok() { com::wrap(super::device::METHODS, d) } else { 0 });
    hr(r)
}

fn set_priority(c: &mut Cpu) -> u32 {
    let (p, v) = res(c);
    unsafe { (v.SetPriority)(p, arg(c, 1)) }
}

fn get_priority(c: &mut Cpu) -> u32 {
    let (p, v) = res(c);
    unsafe { (v.GetPriority)(p) }
}

fn pre_load(c: &mut Cpu) -> u32 {
    let (p, v) = res(c);
    unsafe { (v.PreLoad)(p) };
    0
}

fn get_type(c: &mut Cpu) -> u32 {
    let (p, v) = res(c);
    unsafe { (v.GetType)(p) }.0 as u32
}

// ---- textures ----

fn tex(c: &Cpu) -> (*mut c_void, &'static IDirect3DTexture9_Vtbl) {
    let p = com::host(arg(c, 0));
    (p, unsafe { vtbl(p) })
}

fn tex_qi(c: &mut Cpu) -> u32 {
    query_interface(c, IDirect3DTexture9::IID, "IDirect3DTexture9")
}

fn set_lod(c: &mut Cpu) -> u32 {
    let (p, v) = tex(c);
    unsafe { (v.base__.SetLOD)(p, arg(c, 1)) }
}

fn get_lod(c: &mut Cpu) -> u32 {
    let (p, v) = tex(c);
    unsafe { (v.base__.GetLOD)(p) }
}

fn get_level_count(c: &mut Cpu) -> u32 {
    let (p, v) = tex(c);
    unsafe { (v.base__.GetLevelCount)(p) }
}

fn get_level_desc(c: &mut Cpu) -> u32 {
    let (p, v) = tex(c);
    hr(unsafe { (v.GetLevelDesc)(p, arg(c, 1), gp(arg(c, 2))) })
}

/// Bytes of one row of `width` pixels (of 4x4 blocks for DXT) in `format`, where known.
pub fn row_bytes(format: u32, width: u32) -> Option<usize> {
    let w = width as usize;
    Some(match format {
        0x3154_5844 => w.div_ceil(4) * 8,                              // DXT1
        0x3254_5844..=0x3554_5844 => w.div_ceil(4) * 16,               // DXT2-5
        36 => w * 8,                                                   // A16B16G16R16
        20 => w * 3,                                                   // R8G8B8
        21 | 22 | 31..=35 | 62..=64 | 71 | 75 | 77 | 79 | 83 => w * 4, // 32-bit
        23..=26 | 29 | 30 | 40 | 51 | 60 | 61 | 70 | 80 | 81 => w * 2, // 16-bit
        27 | 28 | 41 | 50 | 52 => w,                                   // 8-bit
        _ => return None,
    })
}

fn is_dxt(format: u32) -> bool {
    (0x3154_5844..=0x3554_5844).contains(&format)
}

/// The bytes a lock of w x h pixels in `format` with `pitch` covers: whole rows of `pitch`
/// but the last, which has only the rect's bytes (rows of 4x4 blocks for DXT). None for a
/// format of unknown size.
pub fn lock_bytes(format: u32, w: u32, h: u32, pitch: usize) -> Option<usize> {
    let rows = if is_dxt(format) { h.div_ceil(4) } else { h } as usize;
    let last = row_bytes(format, w)?.min(pitch);
    Some(if rows == 0 { 0 } else { (rows - 1) * pitch + last })
}

pub const D3DERR_INVALIDCALL: u32 = 0x8876_086c;

/// A rectangle lock of a level or surface described by `desc`: the guest buffer holds the
/// locked rows with the host's pitch. `host_lock` locks the host's; the result goes to the
/// game's D3DLOCKED_RECT at `out`.
fn lock_rect_common(
    this: u32,
    level: u32,
    desc: &D3DSURFACE_DESC,
    out: u32,
    rect: u32,
    flags: u32,
    host_lock: impl FnOnce(&mut D3DLOCKED_RECT) -> windows::core::HRESULT,
) -> u32 {
    let m = mem();
    let format = desc.Format.0;
    let (w, h) = if rect == 0 {
        (desc.Width, desc.Height)
    } else {
        (m.rd32(rect + 8).wrapping_sub(m.rd32(rect)), m.rd32(rect + 12).wrapping_sub(m.rd32(rect + 4)))
    };
    // a part of a surface whose bytes per row are unknown cannot be copied safely
    if rect != 0 && row_bytes(format, w).is_none() {
        log!("warn: LockRect of a part of a surface in format {format:#x}: not supported (D3DERR_INVALIDCALL)");
        return D3DERR_INVALIDCALL;
    }
    let mut lr = D3DLOCKED_RECT::default();
    let r = host_lock(&mut lr);
    if r.is_err() {
        return hr(r);
    }
    let pitch = lr.Pitch as usize;
    // a whole level of unknown format: all its rows of pitch bytes
    let size = lock_bytes(format, w, h, pitch).unwrap_or(h as usize * pitch);
    let guest = com::shadow(this, level, size as u32);
    begin_lock(this, level, guest, lr.pBits, size, flags);
    m.wr32(out, lr.Pitch as u32);
    m.wr32(out + 4, guest);
    0
}

/// IDirect3DTexture9::LockRect(level, locked rect, rect, flags).
fn lock_rect(c: &mut Cpu) -> u32 {
    let (p, v) = tex(c);
    let (this, level, out, rect, flags) = (arg(c, 0), arg(c, 1), arg(c, 2), arg(c, 3), arg(c, 4));
    let mut desc = D3DSURFACE_DESC::default();
    let r = unsafe { (v.GetLevelDesc)(p, level, &mut desc) };
    if r.is_err() {
        return hr(r);
    }
    lock_rect_common(this, level, &desc, out, rect, flags, |lr| unsafe { (v.LockRect)(p, level, lr, cp(rect), flags) })
}

fn get_surface_level(c: &mut Cpu) -> u32 {
    let (p, v) = tex(c);
    let mut s: *mut c_void = std::ptr::null_mut();
    let r = unsafe { (v.GetSurfaceLevel)(p, arg(c, 1), &mut s) };
    created(r, s, SURFACE, arg(c, 2), "GetSurfaceLevel")
}

fn unlock_rect(c: &mut Cpu) -> u32 {
    let (p, v) = tex(c);
    let level = arg(c, 1);
    end_lock(arg(c, 0), level);
    hr(unsafe { (v.UnlockRect)(p, level) })
}

fn add_dirty_rect(c: &mut Cpu) -> u32 {
    let (p, v) = tex(c);
    hr(unsafe { (v.AddDirtyRect)(p, cp(arg(c, 1))) })
}

/// IDirect3DTexture9, in the SDK's order (22 methods).
pub static TEXTURE: &[Import] = &[
    ("IDirect3DTexture9::QueryInterface", 3, tex_qi),
    ("IDirect3DTexture9::AddRef", 1, add_ref),
    ("IDirect3DTexture9::Release", 1, release),
    ("IDirect3DTexture9::GetDevice", 2, get_device),
    ("IDirect3DTexture9::SetPrivateData", 5, NI),
    ("IDirect3DTexture9::GetPrivateData", 4, NI),
    ("IDirect3DTexture9::FreePrivateData", 2, NI),
    ("IDirect3DTexture9::SetPriority", 2, set_priority),
    ("IDirect3DTexture9::GetPriority", 1, get_priority),
    ("IDirect3DTexture9::PreLoad", 1, pre_load),
    ("IDirect3DTexture9::GetType", 1, get_type),
    ("IDirect3DTexture9::SetLOD", 2, set_lod),
    ("IDirect3DTexture9::GetLOD", 1, get_lod),
    ("IDirect3DTexture9::GetLevelCount", 1, get_level_count),
    ("IDirect3DTexture9::SetAutoGenFilterType", 2, NI),
    ("IDirect3DTexture9::GetAutoGenFilterType", 1, NI),
    ("IDirect3DTexture9::GenerateMipSubLevels", 1, NI),
    ("IDirect3DTexture9::GetLevelDesc", 3, get_level_desc),
    ("IDirect3DTexture9::GetSurfaceLevel", 3, get_surface_level),
    ("IDirect3DTexture9::LockRect", 5, lock_rect),
    ("IDirect3DTexture9::UnlockRect", 2, unlock_rect),
    ("IDirect3DTexture9::AddDirtyRect", 2, add_dirty_rect),
];

// ---- surfaces ----

fn surf(c: &Cpu) -> (*mut c_void, &'static IDirect3DSurface9_Vtbl) {
    let p = com::host(arg(c, 0));
    (p, unsafe { vtbl(p) })
}

fn surface_qi(c: &mut Cpu) -> u32 {
    query_interface(c, IDirect3DSurface9::IID, "IDirect3DSurface9")
}

/// GetContainer(riid, out): the texture or device the surface belongs to.
fn get_container(c: &mut Cpu) -> u32 {
    let (p, v) = surf(c);
    let iid: windows::core::GUID = super::types::read_guest(mem(), arg(c, 1));
    let methods = if iid == IDirect3DTexture9::IID {
        TEXTURE
    } else if iid == IDirect3DDevice9::IID {
        super::device::METHODS
    } else {
        log!("warn: IDirect3DSurface9::GetContainer({iid:?}): not wrapped");
        return super::E_NOINTERFACE;
    };
    let mut o: *mut c_void = std::ptr::null_mut();
    let r = unsafe { (v.GetContainer)(p, &iid, &mut o) };
    created(r, o, methods, arg(c, 2), "GetContainer")
}

fn surface_get_desc(c: &mut Cpu) -> u32 {
    let (p, v) = surf(c);
    hr(unsafe { (v.GetDesc)(p, gp(arg(c, 1))) })
}

/// IDirect3DSurface9::LockRect(locked rect, rect, flags).
fn surface_lock_rect(c: &mut Cpu) -> u32 {
    let (p, v) = surf(c);
    let (this, out, rect, flags) = (arg(c, 0), arg(c, 1), arg(c, 2), arg(c, 3));
    let mut desc = D3DSURFACE_DESC::default();
    let r = unsafe { (v.GetDesc)(p, &mut desc) };
    if r.is_err() {
        return hr(r);
    }
    lock_rect_common(this, 0, &desc, out, rect, flags, |lr| unsafe { (v.LockRect)(p, lr, cp(rect), flags) })
}

fn surface_unlock_rect(c: &mut Cpu) -> u32 {
    let (p, v) = surf(c);
    end_lock(arg(c, 0), 0);
    hr(unsafe { (v.UnlockRect)(p) })
}

/// IDirect3DSurface9, in the SDK's order (17 methods).
pub static SURFACE: &[Import] = &[
    ("IDirect3DSurface9::QueryInterface", 3, surface_qi),
    ("IDirect3DSurface9::AddRef", 1, add_ref),
    ("IDirect3DSurface9::Release", 1, release),
    ("IDirect3DSurface9::GetDevice", 2, get_device),
    ("IDirect3DSurface9::SetPrivateData", 5, NI),
    ("IDirect3DSurface9::GetPrivateData", 4, NI),
    ("IDirect3DSurface9::FreePrivateData", 2, NI),
    ("IDirect3DSurface9::SetPriority", 2, set_priority),
    ("IDirect3DSurface9::GetPriority", 1, get_priority),
    ("IDirect3DSurface9::PreLoad", 1, pre_load),
    ("IDirect3DSurface9::GetType", 1, get_type),
    ("IDirect3DSurface9::GetContainer", 3, get_container),
    ("IDirect3DSurface9::GetDesc", 2, surface_get_desc),
    ("IDirect3DSurface9::LockRect", 4, surface_lock_rect),
    ("IDirect3DSurface9::UnlockRect", 1, surface_unlock_rect),
    ("IDirect3DSurface9::GetDC", 2, NI),
    ("IDirect3DSurface9::ReleaseDC", 2, NI),
];

// ---- vertex and index buffers ----

/// Vertex and index buffers have the same methods (and the same vtable layout).
fn buf(c: &Cpu) -> (*mut c_void, &'static IDirect3DVertexBuffer9_Vtbl) {
    let p = com::host(arg(c, 0));
    (p, unsafe { vtbl(p) })
}

fn vb_qi(c: &mut Cpu) -> u32 {
    query_interface(c, IDirect3DVertexBuffer9::IID, "IDirect3DVertexBuffer9")
}

fn ib_qi(c: &mut Cpu) -> u32 {
    query_interface(c, IDirect3DIndexBuffer9::IID, "IDirect3DIndexBuffer9")
}

/// The buffer's size (D3DVERTEXBUFFER_DESC and D3DINDEXBUFFER_DESC both have it at 16).
fn buffer_size(p: *mut c_void, v: &IDirect3DVertexBuffer9_Vtbl) -> Result<u32, windows::core::HRESULT> {
    let mut d = [0u32; 6];
    let r = unsafe { (v.GetDesc)(p, d.as_mut_ptr() as *mut D3DVERTEXBUFFER_DESC) };
    if r.is_err() { Err(r) } else { Ok(d[4]) }
}

/// Lock(offset, size, out pointer, flags): size 0 is the rest of the buffer. The guest buffer
/// covers the whole buffer; the game gets the lock's offset in it.
fn buffer_lock(c: &mut Cpu) -> u32 {
    let (p, v) = buf(c);
    let (this, offset, size, out, flags) = (arg(c, 0), arg(c, 1), arg(c, 2), arg(c, 3), arg(c, 4));
    let total = match buffer_size(p, v) {
        Ok(t) => t,
        Err(r) => {
            log!("warn: buffer Lock: GetDesc failed: {:#x}", r.0);
            return hr(r);
        }
    };
    let size = if size == 0 { total.saturating_sub(offset) } else { size };
    if offset as u64 + size as u64 > total as u64 {
        log!("warn: buffer Lock({offset:#x}, {size:#x}) past its end {total:#x} (D3DERR_INVALIDCALL)");
        return D3DERR_INVALIDCALL;
    }
    let mut data: *mut c_void = std::ptr::null_mut();
    let r = unsafe { (v.Lock)(p, offset, size, &mut data, flags) };
    if r.is_err() {
        return hr(r);
    }
    let guest = com::shadow(this, 0, total) + offset;
    begin_lock(this, 0, guest, data, size as usize, flags);
    mem().wr32(out, guest);
    0
}

fn buffer_unlock(c: &mut Cpu) -> u32 {
    let (p, v) = buf(c);
    end_lock(arg(c, 0), 0);
    hr(unsafe { (v.Unlock)(p) })
}

fn buffer_get_desc(c: &mut Cpu) -> u32 {
    let (p, v) = buf(c);
    hr(unsafe { (v.GetDesc)(p, gp(arg(c, 1))) })
}

macro_rules! buffer_methods {
    ($name:literal, $qi:ident) => {
        &[
            (concat!($name, "::QueryInterface"), 3, $qi),
            (concat!($name, "::AddRef"), 1, add_ref),
            (concat!($name, "::Release"), 1, release),
            (concat!($name, "::GetDevice"), 2, get_device),
            (concat!($name, "::SetPrivateData"), 5, NI),
            (concat!($name, "::GetPrivateData"), 4, NI),
            (concat!($name, "::FreePrivateData"), 2, NI),
            (concat!($name, "::SetPriority"), 2, set_priority),
            (concat!($name, "::GetPriority"), 1, get_priority),
            (concat!($name, "::PreLoad"), 1, pre_load),
            (concat!($name, "::GetType"), 1, get_type),
            (concat!($name, "::Lock"), 5, buffer_lock),
            (concat!($name, "::Unlock"), 1, buffer_unlock),
            (concat!($name, "::GetDesc"), 2, buffer_get_desc),
        ]
    };
}

/// IDirect3DVertexBuffer9, in the SDK's order (14 methods).
pub static VERTEX_BUFFER: &[Import] = buffer_methods!("IDirect3DVertexBuffer9", vb_qi);
/// IDirect3DIndexBuffer9, in the SDK's order (14 methods).
pub static INDEX_BUFFER: &[Import] = buffer_methods!("IDirect3DIndexBuffer9", ib_qi);

// ---- queries ----

fn query(c: &Cpu) -> (*mut c_void, &'static IDirect3DQuery9_Vtbl) {
    let p = com::host(arg(c, 0));
    (p, unsafe { vtbl(p) })
}

fn query_qi(c: &mut Cpu) -> u32 {
    query_interface(c, IDirect3DQuery9::IID, "IDirect3DQuery9")
}

fn query_get_device(c: &mut Cpu) -> u32 {
    let (p, v) = query(c);
    let mut d: *mut c_void = std::ptr::null_mut();
    let r = unsafe { (v.GetDevice)(p, &mut d) };
    mem().wr32(arg(c, 1), if r.is_ok() { com::wrap(super::device::METHODS, d) } else { 0 });
    hr(r)
}

fn query_get_type(c: &mut Cpu) -> u32 {
    let (p, v) = query(c);
    unsafe { (v.GetType)(p) }.0 as u32
}

fn query_get_data_size(c: &mut Cpu) -> u32 {
    let (p, v) = query(c);
    unsafe { (v.GetDataSize)(p) }
}

fn query_issue(c: &mut Cpu) -> u32 {
    let (p, v) = query(c);
    hr(unsafe { (v.Issue)(p, arg(c, 1)) })
}

/// GetData(data, size, flags): the query results (occlusion counts, event flags) hold no
/// pointers.
fn query_get_data(c: &mut Cpu) -> u32 {
    let (p, v) = query(c);
    hr(unsafe { (v.GetData)(p, gp(arg(c, 1)), arg(c, 2), arg(c, 3)) })
}

/// IDirect3DQuery9, in the SDK's order (8 methods).
pub static QUERY: &[Import] = &[
    ("IDirect3DQuery9::QueryInterface", 3, query_qi),
    ("IDirect3DQuery9::AddRef", 1, add_ref),
    ("IDirect3DQuery9::Release", 1, release),
    ("IDirect3DQuery9::GetDevice", 2, query_get_device),
    ("IDirect3DQuery9::GetType", 1, query_get_type),
    ("IDirect3DQuery9::GetDataSize", 1, query_get_data_size),
    ("IDirect3DQuery9::Issue", 2, query_issue),
    ("IDirect3DQuery9::GetData", 4, query_get_data),
];

/// The host interface behind a guest one the game passes (null for 0).
pub fn host_of(guest: u32) -> *mut c_void {
    if guest == 0 { std::ptr::null_mut() } else { com::host(guest) }
}

/// Wrap a host interface the device created into the game's out pointer (0 on failure).
pub fn created(r: windows::core::HRESULT, host: *mut c_void, methods: &'static [Import], out: u32, what: &str) -> u32 {
    if r.is_err() {
        log!("warn: {what} failed: {:#x}", r.0);
        if out != 0 {
            mem().wr32(out, 0);
        }
        return hr(r);
    }
    if out != 0 {
        mem().wr32(out, com::wrap(methods, host));
    }
    0
}
