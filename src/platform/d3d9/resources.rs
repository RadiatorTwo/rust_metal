//! Direct3D 9 resources: textures (plain and cube), surfaces, vertex and index buffers,
//! queries, vertex declarations, vertex and pixel shaders, state blocks. The game loads its
//! textures (DXT, through its own loader 0x479e80) and fills buffers right after CreateDevice;
//! a resource it cannot create ends the game (0x47ab30). The front end's sky is a cube texture
//! (CreateCubeTexture, filled through GetCubeMapSurface and LockRect of each face).
//!
//! A lock gives the game a guest buffer (`com::shadow`) with the host's pitch, one per object
//! and level (a cube face's level is its own): a buffer's covers the whole buffer, a texture
//! level's the whole level, and a lock's range or rect is at its offset in it, so nested locks
//! of different sizes share it. The host's resource stays locked until the game unlocks, then
//! the guest bytes are copied to it. Locks may nest (D3D9 counts them): each object and level
//! keeps a stack of open locks, and each Unlock copies the range of the latest. The host's
//! contents are copied in at the lock unless the lock discards them (`lock_copy_in`), and not
//! copied back for D3DLOCK_READONLY.
//!
//! Buffers keep their guest copy: between locks it holds what the host has, because only our
//! Unlock writes a buffer (the game's dynamic buffers are write-only in the default pool, where
//! reading the host's write-combined memory every frame would be slow and gives undefined
//! bytes). So a buffer's bytes are copied in only at the first lock of each range; afterwards
//! the guest copy is the source of truth. What else writes a buffer makes its copy stale
//! (`stale`): ProcessVertices into it. Reset keeps only managed buffers, whose contents survive.
//! Textures and surfaces are copied in at every lock (the GPU writes render targets, and a
//! texture level can be locked through the texture and through its surface, two copies).
//!
//! Shaders: the game assembles them with its built-in D3DX9; CreateVertexShader and
//! CreatePixelShader get its bytecode as it is (`device`). CreateVertexDeclaration's
//! D3DVERTEXELEMENT9 array (8 bytes each, the same layout) is copied up to its end marker.
//!
//! Not here: volume textures, swap chains, GetDC (logged at their first call).

use super::{add_ref, cp, gp, hr, query_interface, vtbl};
use crate::platform::{Import, arg, com, lock, log, mem, method_not_implemented as NI};
use crate::rt::Cpu;
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::Mutex;
use windows::Win32::Graphics::Direct3D9::*;
use windows::core::Interface;

pub const D3DLOCK_READONLY: u32 = 0x10;
pub const D3DLOCK_NOOVERWRITE: u32 = 0x1000;
pub const D3DLOCK_DISCARD: u32 = 0x2000;

/// Whether a lock with `flags` copies the host's contents into the guest buffer: not when
/// they are discarded. (A buffer copies only what its guest copy does not hold yet, see the
/// module's comment.)
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

/// The locks of one object and level: those open, the latest last, and for a buffer the guest
/// ranges that hold the host's current bytes (sorted, disjoint).
#[derive(Default)]
struct Level {
    open: Vec<Locked>,
    kept: Option<Vec<(u64, u64)>>,
    /// something other than our Unlock wrote the host's buffer once (`stale`)
    stale: bool,
}

/// Lock state by (guest object, level).
static LOCKS: Mutex<Option<HashMap<(u32, u32), Level>>> = Mutex::new(None);

/// Start a lock of (`this`, `level`): `size` host bytes at `host` are the guest's at `guest`,
/// copied there unless the lock discards them. Bytes another open lock of it already covers
/// are not copied: the guest buffer holds them as the game wrote them through that lock (in
/// D3D9 both locks' pointers are the same memory).
pub fn begin_lock(this: u32, level: u32, guest: u32, host: *mut c_void, size: usize, flags: u32) {
    begin(this, level, guest, host, size, flags, false)
}

/// Start a lock of a buffer, whose guest copy is kept between locks: only bytes it never held
/// are copied in.
pub fn begin_kept_lock(this: u32, level: u32, guest: u32, host: *mut c_void, size: usize, flags: u32) {
    begin(this, level, guest, host, size, flags, true)
}

fn begin(this: u32, level: u32, guest: u32, host: *mut c_void, size: usize, flags: u32, keep: bool) {
    let mut locks = lock(&LOCKS);
    let l = locks.get_or_insert_with(HashMap::new).entry((this, level)).or_default();
    if keep && l.kept.is_none() {
        l.kept = Some(Vec::new());
    }
    let (lo, hi) = (guest as u64, guest as u64 + size as u64);
    // a lock the host has not seen (null): nothing to copy from
    if lock_copy_in(flags) && !host.is_null() {
        let open = l.open.iter().map(|l| (l.guest as u64, l.guest as u64 + l.size as u64));
        let mut covered: Vec<(u64, u64)> = open.chain(l.kept.iter().flatten().copied()).filter(|&(a, b)| a < hi && b > lo).collect();
        covered.sort_unstable();
        // the gaps between the covered ranges
        let mut at = lo;
        for (a, b) in covered.into_iter().chain([(hi, hi)]) {
            if a > at {
                let n = (a.min(hi) - at) as usize;
                let src = unsafe { std::slice::from_raw_parts((host as *const u8).add((at - lo) as usize), n) };
                mem().slice(at as u32, n).copy_from_slice(src);
            }
            at = at.max(b);
        }
    }
    // a discarded range is the guest's from now on too: the Unlock writes it
    if let Some(k) = l.kept.as_mut()
        && hi > lo
    {
        k.push((lo, hi));
        k.sort_unstable();
        let mut merged: Vec<(u64, u64)> = Vec::with_capacity(k.len());
        for &(a, b) in k.iter() {
            match merged.last_mut() {
                Some(m) if a <= m.1 => m.1 = m.1.max(b),
                _ => merged.push((a, b)),
            }
        }
        *k = merged;
    }
    l.open.push(Locked { host: host as usize, guest, size, flags });
}

/// End the latest lock of (`this`, `level`): its guest bytes to the host (unless read-only).
/// False if there was none.
pub fn end_lock(this: u32, level: u32) -> bool {
    let Some(l) = lock(&LOCKS).as_mut().and_then(|m| m.get_mut(&(this, level))).and_then(|v| v.open.pop()) else {
        return false;
    };
    if l.size == 0 {
        return true;
    }
    if l.flags & D3DLOCK_READONLY == 0 && l.host != 0 {
        unsafe { std::slice::from_raw_parts_mut(l.host as *mut u8, l.size) }.copy_from_slice(mem().slice(l.guest, l.size));
    }
    true
}

/// Whether a buffer lock of [lo, hi) with `flags` must lock the host's buffer (and so wait for
/// the D3D9 worker): not when it discards, or when the guest copy already holds every byte of
/// the range (`kept`, sorted and disjoint), which then needs nothing from the host.
pub fn lock_needs_host(kept: &[(u64, u64)], lo: u64, hi: u64, flags: u32) -> bool {
    if flags & D3DLOCK_DISCARD != 0 {
        return false;
    }
    let mut at = lo;
    for &(a, b) in kept {
        if a <= at && b > at {
            at = b;
        }
    }
    at < hi
}

/// Whether a buffer's lock of [lo, hi) with `flags` must lock the host's buffer. Only our
/// Unlock writes a buffer, so bytes its guest copy never held were never written: their
/// contents are undefined, and the guest copy's are as good. Not so once something else wrote
/// it (`stale`): then `lock_needs_host` decides by the kept ranges.
fn buffer_needs_host(this: u32, lo: u64, hi: u64, flags: u32) -> bool {
    let locks = lock(&LOCKS);
    match locks.as_ref().and_then(|m| m.get(&(this, 0))) {
        Some(l) if l.stale => lock_needs_host(l.kept.as_deref().unwrap_or(&[]), lo, hi, flags),
        _ => false,
    }
}

/// The latest open lock of (`this`, `level`): (host pointer, 0 for a lock the host has not
/// seen yet; guest address; size; flags).
fn top_lock(this: u32, level: u32) -> Option<(usize, u32, usize, u32)> {
    lock(&LOCKS).as_ref().and_then(|m| m.get(&(this, level))).and_then(|l| l.open.last()).map(|l| (l.host, l.guest, l.size, l.flags))
}

/// Something other than our Unlock wrote the host's resource: a kept guest copy is stale.
pub fn stale(this: u32) {
    if let Some(m) = lock(&LOCKS).as_mut() {
        for (_, l) in m.iter_mut().filter(|(k, _)| k.0 == this) {
            if let Some(k) = l.kept.as_mut() {
                k.clear();
            }
            l.stale = true;
        }
    }
}

/// The guest object is gone: its lock state too (a new object may get its address).
pub fn forget(this: u32) {
    if let Some(m) = lock(&LOCKS).as_mut() {
        m.retain(|k, _| k.0 != this);
    }
}

/// IUnknown::Release of a lockable resource: at 0 its lock state goes with it.
fn release(c: &mut Cpu) -> u32 {
    let this = arg(c, 0);
    let host = com::host(this);
    let n = super::release(c);
    if n == 0 {
        forget(this);
        super::ex::released(host);
    }
    n
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

/// SetPrivateData(guid, data, size, flags): plain bytes are copied by D3D9; an interface
/// (D3DSPD_IUNKNOWN) would be a guest object it cannot hold.
fn set_private_data(c: &mut Cpu) -> u32 {
    let (p, v) = res(c);
    if arg(c, 4) & 1 != 0 {
        log!("warn: SetPrivateData with an interface: not supported (D3DERR_INVALIDCALL)");
        return D3DERR_INVALIDCALL;
    }
    hr(unsafe { (v.SetPrivateData)(p, cp(arg(c, 1)), cp(arg(c, 2)), arg(c, 3), 0) })
}

fn get_private_data(c: &mut Cpu) -> u32 {
    let (p, v) = res(c);
    hr(unsafe { (v.GetPrivateData)(p, cp(arg(c, 1)), gp(arg(c, 2)), gp(arg(c, 3))) })
}

fn free_private_data(c: &mut Cpu) -> u32 {
    let (p, v) = res(c);
    hr(unsafe { (v.FreePrivateData)(p, cp(arg(c, 1))) })
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

fn set_auto_gen_filter_type(c: &mut Cpu) -> u32 {
    let (p, v) = tex(c);
    hr(unsafe { (v.base__.SetAutoGenFilterType)(p, D3DTEXTUREFILTERTYPE(arg(c, 1) as i32)) })
}

fn get_auto_gen_filter_type(c: &mut Cpu) -> u32 {
    let (p, v) = tex(c);
    unsafe { (v.base__.GetAutoGenFilterType)(p) }.0 as u32
}

fn generate_mip_sub_levels(c: &mut Cpu) -> u32 {
    let (p, v) = tex(c);
    unsafe { (v.base__.GenerateMipSubLevels)(p) };
    0
}

fn get_level_desc(c: &mut Cpu) -> u32 {
    super::ex::fix_pool_after(com::host(arg(c, 0)), arg(c, 2), || get_level_desc_host(c))
}

fn get_level_desc_host(c: &mut Cpu) -> u32 {
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

/// Where a rect from (`left`, `top`) starts in a level of `format` with `pitch` bytes per row
/// (rows of 4x4 blocks for DXT). None for a format of unknown size.
pub fn rect_offset(format: u32, left: u32, top: u32, pitch: usize) -> Option<usize> {
    let rows = if is_dxt(format) { top / 4 } else { top } as usize;
    Some(rows * pitch + row_bytes(format, left)?)
}

/// A rectangle lock of a level or surface described by `desc`, `key` its guest buffer (the
/// level, or face and level): the buffer holds the whole level with the host's pitch, and the
/// game gets the rect's place in it. `host_lock` locks the host's; the result goes to the
/// game's D3DLOCKED_RECT at `out`.
#[allow(clippy::too_many_arguments)]
fn lock_rect_common(
    this: u32,
    key: u32,
    desc: &D3DSURFACE_DESC,
    out: u32,
    rect: u32,
    flags: u32,
    host_lock: impl FnOnce(&mut D3DLOCKED_RECT) -> windows::core::HRESULT,
    host_unlock: impl FnOnce(),
) -> u32 {
    let m = mem();
    let format = desc.Format.0;
    let (left, top, w, h) = if rect == 0 {
        (0, 0, desc.Width, desc.Height)
    } else {
        let (l, t) = (m.rd32(rect), m.rd32(rect + 4));
        (l, t, m.rd32(rect + 8).wrapping_sub(l), m.rd32(rect + 12).wrapping_sub(t))
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
    // a whole level of unknown format: all its rows of pitch bytes (too many for an unknown
    // block-compressed FourCC, but within the host's allocation)
    let rows_of_pitch = |h: u32| {
        static SEEN: Mutex<Vec<u32>> = Mutex::new(Vec::new());
        let mut seen = lock(&SEEN);
        if !seen.contains(&format) {
            seen.push(format);
            log!("warn: LockRect of format {format:#x} (size unknown): {h} rows of the pitch copied");
        }
        h as usize * pitch
    };
    let size = lock_bytes(format, w, h, pitch).unwrap_or_else(|| rows_of_pitch(h));
    let level = lock_bytes(format, desc.Width, desc.Height, pitch).unwrap_or(desc.Height as usize * pitch);
    let at = if rect == 0 { 0 } else { rect_offset(format, left, top, pitch).unwrap_or(usize::MAX) };
    if at.checked_add(size).is_none_or(|e| e > level) {
        log!("warn: LockRect of {w}x{h} at ({left}, {top}) past the level's {} bytes (D3DERR_INVALIDCALL)", level);
        host_unlock();
        return D3DERR_INVALIDCALL;
    }
    let guest = com::shadow(this, key, level as u32) + at as u32;
    begin_lock(this, key, guest, lr.pBits, size, flags);
    m.wr32(out, lr.Pitch as u32);
    m.wr32(out + 4, guest);
    0
}

/// IDirect3DTexture9::LockRect(level, locked rect, rect, flags).
fn lock_rect(c: &mut Cpu) -> u32 {
    let (p, v) = tex(c);
    let (this, level, out, rect, mut flags) = (arg(c, 0), arg(c, 1), arg(c, 2), arg(c, 3), arg(c, 4));
    // a managed texture on 9Ex: its twin in system memory (no discarding there)
    let p = match super::ex::twin(p) {
        Some(t) => {
            flags &= !(D3DLOCK_DISCARD | D3DLOCK_NOOVERWRITE);
            t
        }
        None => p,
    };
    let mut desc = D3DSURFACE_DESC::default();
    let r = unsafe { (v.GetLevelDesc)(p, level, &mut desc) };
    if r.is_err() {
        return hr(r);
    }
    lock_rect_common(this, level, &desc, out, rect, flags, |lr| unsafe { (v.LockRect)(p, level, lr, cp(rect), flags) }, || unsafe {
        let _ = (v.UnlockRect)(p, level);
    })
}

fn get_surface_level(c: &mut Cpu) -> u32 {
    let (p, v) = tex(c);
    let mut s: *mut c_void = std::ptr::null_mut();
    let r = unsafe { (v.GetSurfaceLevel)(p, arg(c, 1), &mut s) };
    if r.is_ok() {
        super::ex::surface_of(s, p, arg(c, 1), false);
    }
    created(r, s, SURFACE, arg(c, 2), "GetSurfaceLevel")
}

fn unlock_rect(c: &mut Cpu) -> u32 {
    let (p, v) = tex(c);
    let level = arg(c, 1);
    end_lock(arg(c, 0), level);
    if let Some(t) = super::ex::twin(p) {
        let r = unsafe { (v.UnlockRect)(t, level) };
        super::ex::upload(p, false);
        return hr(r);
    }
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
    ("IDirect3DTexture9::SetPrivateData", 5, set_private_data),
    ("IDirect3DTexture9::GetPrivateData", 4, get_private_data),
    ("IDirect3DTexture9::FreePrivateData", 2, free_private_data),
    ("IDirect3DTexture9::SetPriority", 2, set_priority),
    ("IDirect3DTexture9::GetPriority", 1, get_priority),
    ("IDirect3DTexture9::PreLoad", 1, pre_load),
    ("IDirect3DTexture9::GetType", 1, get_type),
    ("IDirect3DTexture9::SetLOD", 2, set_lod),
    ("IDirect3DTexture9::GetLOD", 1, get_lod),
    ("IDirect3DTexture9::GetLevelCount", 1, get_level_count),
    ("IDirect3DTexture9::SetAutoGenFilterType", 2, set_auto_gen_filter_type),
    ("IDirect3DTexture9::GetAutoGenFilterType", 1, get_auto_gen_filter_type),
    ("IDirect3DTexture9::GenerateMipSubLevels", 1, generate_mip_sub_levels),
    ("IDirect3DTexture9::GetLevelDesc", 3, get_level_desc),
    ("IDirect3DTexture9::GetSurfaceLevel", 3, get_surface_level),
    ("IDirect3DTexture9::LockRect", 5, lock_rect),
    ("IDirect3DTexture9::UnlockRect", 2, unlock_rect),
    ("IDirect3DTexture9::AddDirtyRect", 2, add_dirty_rect),
];

// ---- cube textures ----

fn cube(c: &Cpu) -> (*mut c_void, &'static IDirect3DCubeTexture9_Vtbl) {
    let p = com::host(arg(c, 0));
    (p, unsafe { vtbl(p) })
}

fn cube_qi(c: &mut Cpu) -> u32 {
    query_interface(c, IDirect3DCubeTexture9::IID, "IDirect3DCubeTexture9")
}

fn cube_get_level_desc(c: &mut Cpu) -> u32 {
    let (p, v) = cube(c);
    super::ex::fix_pool_after(p, arg(c, 2), || hr(unsafe { (v.GetLevelDesc)(p, arg(c, 1), gp(arg(c, 2))) }))
}

/// The guest buffer key of a cube face's level.
fn face_key(face: u32, level: u32) -> u32 {
    face << 16 | level
}

/// GetCubeMapSurface(face, level, out).
fn get_cube_map_surface(c: &mut Cpu) -> u32 {
    let (p, v) = cube(c);
    let mut s: *mut c_void = std::ptr::null_mut();
    let r = unsafe { (v.GetCubeMapSurface)(p, D3DCUBEMAP_FACES(arg(c, 1) as i32), arg(c, 2), &mut s) };
    if r.is_ok() {
        super::ex::surface_of(s, p, face_key(arg(c, 1), arg(c, 2)), true);
    }
    created(r, s, SURFACE, arg(c, 3), "GetCubeMapSurface")
}

/// LockRect(face, level, locked rect, rect, flags).
fn cube_lock_rect(c: &mut Cpu) -> u32 {
    let (p, v) = cube(c);
    let (this, face, level, out, rect, mut flags) = (arg(c, 0), arg(c, 1), arg(c, 2), arg(c, 3), arg(c, 4), arg(c, 5));
    let p = match super::ex::twin(p) {
        Some(t) => {
            flags &= !(D3DLOCK_DISCARD | D3DLOCK_NOOVERWRITE);
            t
        }
        None => p,
    };
    let f = D3DCUBEMAP_FACES(face as i32);
    let mut desc = D3DSURFACE_DESC::default();
    let r = unsafe { (v.GetLevelDesc)(p, level, &mut desc) };
    if r.is_err() {
        return hr(r);
    }
    lock_rect_common(this, face_key(face, level), &desc, out, rect, flags, |lr| unsafe { (v.LockRect)(p, f, level, lr, cp(rect), flags) }, || unsafe {
        let _ = (v.UnlockRect)(p, f, level);
    })
}

fn cube_unlock_rect(c: &mut Cpu) -> u32 {
    let (p, v) = cube(c);
    let (face, level) = (arg(c, 1), arg(c, 2));
    end_lock(arg(c, 0), face_key(face, level));
    if let Some(t) = super::ex::twin(p) {
        let r = unsafe { (v.UnlockRect)(t, D3DCUBEMAP_FACES(face as i32), level) };
        super::ex::upload(p, true);
        return hr(r);
    }
    hr(unsafe { (v.UnlockRect)(p, D3DCUBEMAP_FACES(face as i32), level) })
}

fn cube_add_dirty_rect(c: &mut Cpu) -> u32 {
    let (p, v) = cube(c);
    hr(unsafe { (v.AddDirtyRect)(p, D3DCUBEMAP_FACES(arg(c, 1) as i32), cp(arg(c, 2))) })
}

/// IDirect3DCubeTexture9, in the SDK's order (22 methods). The base texture methods are in
/// the same slots as a texture's.
pub static CUBE_TEXTURE: &[Import] = &[
    ("IDirect3DCubeTexture9::QueryInterface", 3, cube_qi),
    ("IDirect3DCubeTexture9::AddRef", 1, add_ref),
    ("IDirect3DCubeTexture9::Release", 1, release),
    ("IDirect3DCubeTexture9::GetDevice", 2, get_device),
    ("IDirect3DCubeTexture9::SetPrivateData", 5, set_private_data),
    ("IDirect3DCubeTexture9::GetPrivateData", 4, get_private_data),
    ("IDirect3DCubeTexture9::FreePrivateData", 2, free_private_data),
    ("IDirect3DCubeTexture9::SetPriority", 2, set_priority),
    ("IDirect3DCubeTexture9::GetPriority", 1, get_priority),
    ("IDirect3DCubeTexture9::PreLoad", 1, pre_load),
    ("IDirect3DCubeTexture9::GetType", 1, get_type),
    ("IDirect3DCubeTexture9::SetLOD", 2, set_lod),
    ("IDirect3DCubeTexture9::GetLOD", 1, get_lod),
    ("IDirect3DCubeTexture9::GetLevelCount", 1, get_level_count),
    ("IDirect3DCubeTexture9::SetAutoGenFilterType", 2, set_auto_gen_filter_type),
    ("IDirect3DCubeTexture9::GetAutoGenFilterType", 1, get_auto_gen_filter_type),
    ("IDirect3DCubeTexture9::GenerateMipSubLevels", 1, generate_mip_sub_levels),
    ("IDirect3DCubeTexture9::GetLevelDesc", 3, cube_get_level_desc),
    ("IDirect3DCubeTexture9::GetCubeMapSurface", 4, get_cube_map_surface),
    ("IDirect3DCubeTexture9::LockRect", 6, cube_lock_rect),
    ("IDirect3DCubeTexture9::UnlockRect", 3, cube_unlock_rect),
    ("IDirect3DCubeTexture9::AddDirtyRect", 3, cube_add_dirty_rect),
];

/// The guest object for a texture the device hands out (GetTexture), by its resource type.
pub(crate) fn wrap_base_texture(host: *mut c_void) -> u32 {
    let ty = unsafe { (vtbl::<IDirect3DResource9_Vtbl>(host).GetType)(host) }.0;
    let methods = match ty {
        5 => CUBE_TEXTURE,
        3 => TEXTURE,
        _ => {
            log!("warn: a texture of resource type {ty} handed out: wrapped as a texture");
            TEXTURE
        }
    };
    com::wrap(methods, host)
}

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
    } else if iid == IDirect3DCubeTexture9::IID {
        CUBE_TEXTURE
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
    super::ex::fix_pool_after(p, arg(c, 1), || hr(unsafe { (v.GetDesc)(p, gp(arg(c, 1))) }))
}

/// IDirect3DSurface9::LockRect(locked rect, rect, flags).
fn surface_lock_rect(c: &mut Cpu) -> u32 {
    let (p, v) = surf(c);
    let (this, out, rect, flags) = (arg(c, 0), arg(c, 1), arg(c, 2), arg(c, 3));
    // a level of a managed texture on 9Ex: the twin's level
    if let Some((t, key, cube)) = super::ex::surface_texture(p) {
        return twin_level_lock(this, super::ex::twin(t).unwrap_or(t), key, cube, out, rect, flags & !(D3DLOCK_DISCARD | D3DLOCK_NOOVERWRITE));
    }
    let mut desc = D3DSURFACE_DESC::default();
    let r = unsafe { (v.GetDesc)(p, &mut desc) };
    if r.is_err() {
        return hr(r);
    }
    lock_rect_common(this, 0, &desc, out, rect, flags, |lr| unsafe { (v.LockRect)(p, lr, cp(rect), flags) }, || unsafe {
        let _ = (v.UnlockRect)(p);
    })
}

fn surface_unlock_rect(c: &mut Cpu) -> u32 {
    let (p, v) = surf(c);
    end_lock(arg(c, 0), 0);
    if let Some((t, key, cube)) = super::ex::surface_texture(p) {
        let twin = super::ex::twin(t).unwrap_or(t);
        let r = unsafe {
            if cube {
                (vtbl::<IDirect3DCubeTexture9_Vtbl>(twin).UnlockRect)(twin, D3DCUBEMAP_FACES((key >> 16) as i32), key & 0xffff)
            } else {
                (vtbl::<IDirect3DTexture9_Vtbl>(twin).UnlockRect)(twin, key)
            }
        };
        super::ex::upload(t, cube);
        return hr(r);
    }
    hr(unsafe { (v.UnlockRect)(p) })
}

/// A lock of a managed texture's level through its surface (`this`, lock key 0): the level of
/// the twin `twin` (`key`: level, or face and level).
fn twin_level_lock(this: u32, twin: *mut c_void, key: u32, cube: bool, out: u32, rect: u32, flags: u32) -> u32 {
    let mut desc = D3DSURFACE_DESC::default();
    unsafe {
        if cube {
            let v: &IDirect3DCubeTexture9_Vtbl = vtbl(twin);
            let (face, level) = (D3DCUBEMAP_FACES((key >> 16) as i32), key & 0xffff);
            let r = (v.GetLevelDesc)(twin, level, &mut desc);
            if r.is_err() {
                return hr(r);
            }
            lock_rect_common(this, 0, &desc, out, rect, flags, |lr| (v.LockRect)(twin, face, level, lr, cp(rect), flags), || {
                let _ = (v.UnlockRect)(twin, face, level);
            })
        } else {
            let v: &IDirect3DTexture9_Vtbl = vtbl(twin);
            let r = (v.GetLevelDesc)(twin, key, &mut desc);
            if r.is_err() {
                return hr(r);
            }
            lock_rect_common(this, 0, &desc, out, rect, flags, |lr| (v.LockRect)(twin, key, lr, cp(rect), flags), || {
                let _ = (v.UnlockRect)(twin, key);
            })
        }
    }
}

/// IDirect3DSurface9, in the SDK's order (17 methods).
pub static SURFACE: &[Import] = &[
    ("IDirect3DSurface9::QueryInterface", 3, surface_qi),
    ("IDirect3DSurface9::AddRef", 1, add_ref),
    ("IDirect3DSurface9::Release", 1, release),
    ("IDirect3DSurface9::GetDevice", 2, get_device),
    ("IDirect3DSurface9::SetPrivateData", 5, set_private_data),
    ("IDirect3DSurface9::GetPrivateData", 4, get_private_data),
    ("IDirect3DSurface9::FreePrivateData", 2, free_private_data),
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
/// covers the whole buffer and is kept between locks; the game gets the lock's offset in it.
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
    // with the D3D9 worker: the guest copy answers, the host's buffer is written at the Unlock
    let (lo, hi) = (offset as u64, offset as u64 + size as u64);
    if super::queue::global().is_some() && !buffer_needs_host(this, lo, hi, flags) {
        let guest = com::shadow(this, 0, total) + offset;
        begin_kept_lock(this, 0, guest, std::ptr::null_mut(), size as usize, flags);
        mem().wr32(out, guest);
        return 0;
    }
    super::queue::flush();
    let mut data: *mut c_void = std::ptr::null_mut();
    let r = unsafe { (v.Lock)(p, offset, size, &mut data, flags) };
    if r.is_err() {
        return hr(r);
    }
    let guest = com::shadow(this, 0, total) + offset;
    begin_kept_lock(this, 0, guest, data, size as usize, flags);
    mem().wr32(out, guest);
    0
}

fn buffer_unlock(c: &mut Cpu) -> u32 {
    let (p, v) = buf(c);
    let this = arg(c, 0);
    // a lock the host has not seen: the bytes go to the worker, which writes them
    if let (Some(q), Some((0, guest, size, flags))) = (super::queue::global(), top_lock(this, 0)) {
        end_lock(this, 0);
        if flags & D3DLOCK_READONLY != 0 || size == 0 {
            return 0;
        }
        let offset = guest - com::shadow(this, 0, 0);
        let bytes: Box<[u8]> = mem().slice(guest, size).into();
        let (p, lock_fn, unlock_fn) = (p as usize, v.Lock, v.Unlock);
        q.record(Box::new(move || unsafe {
            let p = p as *mut c_void;
            let mut data: *mut c_void = std::ptr::null_mut();
            let r = lock_fn(p, offset, size as u32, &mut data, flags & !D3DLOCK_READONLY);
            if r.is_err() || data.is_null() {
                log!("d3d9 worker: buffer Lock({offset:#x}, {size:#x}) -> {:#x}", r.0);
                return;
            }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), data as *mut u8, size);
            let _ = unlock_fn(p);
        }));
        return 0;
    }
    super::queue::flush();
    end_lock(this, 0);
    hr(unsafe { (v.Unlock)(p) })
}

fn buffer_get_desc(c: &mut Cpu) -> u32 {
    let (p, v) = buf(c);
    super::ex::fix_pool_after(p, arg(c, 1), || hr(unsafe { (v.GetDesc)(p, gp(arg(c, 1))) }))
}

macro_rules! buffer_methods {
    ($name:literal, $qi:ident) => {
        &[
            (concat!($name, "::QueryInterface"), 3, $qi),
            (concat!($name, "::AddRef"), 1, add_ref),
            (concat!($name, "::Release"), 1, release),
            (concat!($name, "::GetDevice"), 2, get_device),
            (concat!($name, "::SetPrivateData"), 5, set_private_data),
            (concat!($name, "::GetPrivateData"), 4, get_private_data),
            (concat!($name, "::FreePrivateData"), 2, free_private_data),
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
    let flags = arg(c, 1);
    if let Some(q) = super::queue::global() {
        let (p, issue) = (p as usize, v.Issue);
        q.record(Box::new(move || {
            let _ = unsafe { issue(p as *mut c_void, flags) };
        }));
        return 0;
    }
    hr(unsafe { (v.Issue)(p, flags) })
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

// ---- vertex declarations, shaders, state blocks ----

/// D3DVERTEXELEMENT9s at `a` up to and with the end marker (stream 0xff), at most 65 (the
/// SDK's MAXD3DDECLLENGTH and the end).
pub fn vertex_elements(a: u32) -> Vec<D3DVERTEXELEMENT9> {
    let m = mem();
    let mut v = Vec::new();
    for k in 0..65 {
        let e = a + 8 * k;
        let b = m.slice(e + 4, 4);
        v.push(D3DVERTEXELEMENT9 { Stream: m.rd16(e), Offset: m.rd16(e + 2), Type: b[0], Method: b[1], Usage: b[2], UsageIndex: b[3] });
        if m.rd16(e) == 0xff {
            break;
        }
    }
    v
}

fn decl_qi(c: &mut Cpu) -> u32 {
    query_interface(c, IDirect3DVertexDeclaration9::IID, "IDirect3DVertexDeclaration9")
}

/// GetDeclaration(elements, count): the same layout.
fn get_declaration(c: &mut Cpu) -> u32 {
    let p = com::host(arg(c, 0));
    let v: &IDirect3DVertexDeclaration9_Vtbl = unsafe { vtbl(p) };
    hr(unsafe { (v.GetDeclaration)(p, gp(arg(c, 1)), gp(arg(c, 2))) })
}

/// IDirect3DVertexDeclaration9 (5 methods).
pub static VERTEX_DECLARATION: &[Import] = &[
    ("IDirect3DVertexDeclaration9::QueryInterface", 3, decl_qi),
    ("IDirect3DVertexDeclaration9::AddRef", 1, add_ref),
    ("IDirect3DVertexDeclaration9::Release", 1, super::release),
    ("IDirect3DVertexDeclaration9::GetDevice", 2, get_device),
    ("IDirect3DVertexDeclaration9::GetDeclaration", 3, get_declaration),
];

fn vs_qi(c: &mut Cpu) -> u32 {
    query_interface(c, IDirect3DVertexShader9::IID, "IDirect3DVertexShader9")
}

fn ps_qi(c: &mut Cpu) -> u32 {
    query_interface(c, IDirect3DPixelShader9::IID, "IDirect3DPixelShader9")
}

/// GetFunction(data, size) of either kind of shader (the same vtable layout).
fn get_function(c: &mut Cpu) -> u32 {
    let p = com::host(arg(c, 0));
    let v: &IDirect3DVertexShader9_Vtbl = unsafe { vtbl(p) };
    hr(unsafe { (v.GetFunction)(p, gp(arg(c, 1)), gp(arg(c, 2))) })
}

/// IDirect3DVertexShader9 (5 methods).
pub static VERTEX_SHADER: &[Import] = &[
    ("IDirect3DVertexShader9::QueryInterface", 3, vs_qi),
    ("IDirect3DVertexShader9::AddRef", 1, add_ref),
    ("IDirect3DVertexShader9::Release", 1, super::release),
    ("IDirect3DVertexShader9::GetDevice", 2, get_device),
    ("IDirect3DVertexShader9::GetFunction", 3, get_function),
];

/// IDirect3DPixelShader9 (5 methods).
pub static PIXEL_SHADER: &[Import] = &[
    ("IDirect3DPixelShader9::QueryInterface", 3, ps_qi),
    ("IDirect3DPixelShader9::AddRef", 1, add_ref),
    ("IDirect3DPixelShader9::Release", 1, super::release),
    ("IDirect3DPixelShader9::GetDevice", 2, get_device),
    ("IDirect3DPixelShader9::GetFunction", 3, get_function),
];

fn sb(c: &Cpu) -> (*mut c_void, &'static IDirect3DStateBlock9_Vtbl) {
    let p = com::host(arg(c, 0));
    (p, unsafe { vtbl(p) })
}

fn sb_qi(c: &mut Cpu) -> u32 {
    query_interface(c, IDirect3DStateBlock9::IID, "IDirect3DStateBlock9")
}

fn sb_capture(c: &mut Cpu) -> u32 {
    let (p, v) = sb(c);
    hr(unsafe { (v.Capture)(p) })
}

fn sb_apply(c: &mut Cpu) -> u32 {
    super::device::invalidate_states();
    let (p, v) = sb(c);
    hr(unsafe { (v.Apply)(p) })
}

/// IDirect3DStateBlock9 (6 methods): it holds host interfaces (textures, buffers, shaders),
/// which Apply sets on the host device as they are.
pub static STATE_BLOCK: &[Import] = &[
    ("IDirect3DStateBlock9::QueryInterface", 3, sb_qi),
    ("IDirect3DStateBlock9::AddRef", 1, add_ref),
    ("IDirect3DStateBlock9::Release", 1, super::release),
    ("IDirect3DStateBlock9::GetDevice", 2, get_device),
    ("IDirect3DStateBlock9::Capture", 1, sb_capture),
    ("IDirect3DStateBlock9::Apply", 1, sb_apply),
];

/// Vertices a draw of `count` primitives of type `ty` (D3DPRIMITIVETYPE) reads.
pub fn primitive_vertices(ty: u32, count: u32) -> u32 {
    if count == 0 {
        return 0;
    }
    match ty {
        1 => count,         // point list
        2 => 2 * count,     // line list
        3 => count + 1,     // line strip
        4 => 3 * count,     // triangle list
        5 | 6 => count + 2, // triangle strip, fan
        _ => 0,
    }
}

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
