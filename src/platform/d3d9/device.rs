//! IDirect3DDevice9: the game's calls go to the system's device. All 119 methods are in the
//! guest vtable in the SDK's order; those that hold only values or structures of the same
//! layout pass straight through, the present parameters are converted (`types`), and every
//! interface the device creates or hands out is wrapped (`resources`): textures, cube
//! textures, surfaces, buffers, queries, vertex declarations, shaders, state blocks. *UP draws
//! give D3D9 the game's own arrays (guest memory is host memory). What is not implemented
//! (swap chains, volume textures, patches, GetNPatchMode, which returns a float) returns 0 and
//! is logged at its first call.
//!
//! `GUNMETAL_SHOT=file.png`: the back buffer is written there before Present (see `shot`).

use super::{add_ref, cp, gp, hr, query_interface, release, resources, vtbl};
use crate::platform::{Import, arg, com, log, mem, method_not_implemented as NI, window};
use crate::rt::Cpu;
use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, Ordering};
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct3D9::*;
use windows::core::{BOOL, Interface};

/// The behaviour flags the game passed to CreateDevice (GetCreationParameters reports these,
/// not ours with D3DCREATE_MULTITHREADED).
pub(super) static GAME_FLAGS: AtomicU32 = AtomicU32::new(0);

fn dv(c: &Cpu) -> (*mut c_void, &'static IDirect3DDevice9_Vtbl) {
    let p = com::host(arg(c, 0));
    (p, unsafe { vtbl(p) })
}

/// A method whose arguments after `this` are given as host values: `pass!(name, Method, |c|
/// (args...))`; returns the HRESULT.
macro_rules! pass {
    ($f:ident, $m:ident, |$c:ident| ($($a:expr),*)) => {
        fn $f($c: &mut Cpu) -> u32 {
            let (p, v) = dv($c);
            hr(unsafe { (v.$m)(p, $($a),*) })
        }
    };
}

/// Release: at 0 the device is gone (`d3d9::device` null).
fn device_release(c: &mut Cpu) -> u32 {
    let host = com::host(arg(c, 0));
    let n = release(c);
    if n == 0 {
        super::device_released(host);
    }
    n
}

fn qi(c: &mut Cpu) -> u32 {
    query_interface(c, IDirect3DDevice9::IID, "IDirect3DDevice9")
}

fn get_available_texture_mem(c: &mut Cpu) -> u32 {
    let (p, v) = dv(c);
    unsafe { (v.GetAvailableTextureMem)(p) }
}

fn get_number_of_swap_chains(c: &mut Cpu) -> u32 {
    let (p, v) = dv(c);
    unsafe { (v.GetNumberOfSwapChains)(p) }
}

fn get_direct3d(c: &mut Cpu) -> u32 {
    let (p, v) = dv(c);
    let mut d: *mut c_void = std::ptr::null_mut();
    let r = unsafe { (v.GetDirect3D)(p, &mut d) };
    mem().wr32(arg(c, 1), if r.is_ok() { com::wrap(super::IDIRECT3D9, d) } else { 0 });
    hr(r)
}

fn get_creation_parameters(c: &mut Cpu) -> u32 {
    let (p, v) = dv(c);
    let mut cp = D3DDEVICE_CREATION_PARAMETERS::default();
    let r = unsafe { (v.GetCreationParameters)(p, &mut cp) };
    if r.is_ok() {
        let a = arg(c, 1);
        let m = mem();
        m.wr32(a, cp.AdapterOrdinal);
        m.wr32(a + 4, cp.DeviceType.0 as u32);
        m.wr32(a + 8, window::guest_hwnd(cp.hFocusWindow.0 as isize));
        m.wr32(a + 12, GAME_FLAGS.load(Ordering::Relaxed));
    }
    hr(r)
}

fn set_cursor_position(c: &mut Cpu) -> u32 {
    let (p, v) = dv(c);
    unsafe { (v.SetCursorPosition)(p, arg(c, 1) as i32, arg(c, 2) as i32, arg(c, 3)) };
    0
}

fn show_cursor(c: &mut Cpu) -> u32 {
    let (p, v) = dv(c);
    unsafe { (v.ShowCursor)(p, BOOL(arg(c, 1) as i32)) }.0 as u32
}

/// Reset(present parameters): a new mode or windowed/fullscreen, as the game asks.
fn reset(c: &mut Cpu) -> u32 {
    let (p, v) = dv(c);
    hr(super::with_present_params(arg(c, 1), super::device_no_16bit_modes(), "Reset", |params| unsafe { (v.Reset)(p, params) }))
}

/// Present(source rect, dest rect, window, dirty region): the back buffer, written to
/// GUNMETAL_SHOT first when one is due. Without a rectangle of the game's, the picture keeps
/// its aspect ratio in our window (`letterbox`).
fn present(c: &mut Cpu) -> u32 {
    let (p, v) = dv(c);
    super::shot::before_present(p);
    let w = arg(c, 3);
    let win = if w == 0 { HWND::default() } else { super::host_hwnd(w) };
    let target = if w == 0 { super::host_hwnd(crate::platform::user32::main_window()) } else { win };
    let dest = if arg(c, 2) == 0 { super::letterbox(target) } else { None };
    let dest_ptr = dest.as_ref().map_or(cp(arg(c, 2)), |r| r as *const _);
    hr(unsafe { (v.Present)(p, cp(arg(c, 1)), dest_ptr, win, cp(arg(c, 4))) })
}

fn set_gamma_ramp(c: &mut Cpu) -> u32 {
    let (p, v) = dv(c);
    unsafe { (v.SetGammaRamp)(p, arg(c, 1), arg(c, 2), cp(arg(c, 3))) };
    0
}

fn get_gamma_ramp(c: &mut Cpu) -> u32 {
    let (p, v) = dv(c);
    unsafe { (v.GetGammaRamp)(p, arg(c, 1), gp(arg(c, 2))) };
    0
}

fn get_software_vertex_processing(c: &mut Cpu) -> u32 {
    let (p, v) = dv(c);
    unsafe { (v.GetSoftwareVertexProcessing)(p) }.0 as u32
}

pass!(test_cooperative_level, TestCooperativeLevel, |c| ());
pass!(evict_managed_resources, EvictManagedResources, |c| ());
pass!(get_device_caps, GetDeviceCaps, |c| (gp(arg(c, 1))));
pass!(get_display_mode, GetDisplayMode, |c| (arg(c, 1), gp(arg(c, 2))));
pass!(get_raster_status, GetRasterStatus, |c| (arg(c, 1), gp(arg(c, 2))));
pass!(set_dialog_box_mode, SetDialogBoxMode, |c| (BOOL(arg(c, 1) as i32)));
pass!(begin_scene, BeginScene, |c| ());
pass!(end_scene, EndScene, |c| ());
// Clear(count, rects, flags, colour, z, stencil): z is a float
pass!(clear, Clear, |c| (arg(c, 1), cp(arg(c, 2)), arg(c, 3), arg(c, 4), f32::from_bits(arg(c, 5)), arg(c, 6)));
pass!(set_transform, SetTransform, |c| (D3DTRANSFORMSTATETYPE(arg(c, 1) as i32), cp(arg(c, 2))));
pass!(get_transform, GetTransform, |c| (D3DTRANSFORMSTATETYPE(arg(c, 1) as i32), gp(arg(c, 2))));
pass!(multiply_transform, MultiplyTransform, |c| (D3DTRANSFORMSTATETYPE(arg(c, 1) as i32), cp(arg(c, 2))));
pass!(set_viewport, SetViewport, |c| (cp(arg(c, 1))));
pass!(get_viewport, GetViewport, |c| (gp(arg(c, 1))));
pass!(set_material, SetMaterial, |c| (cp(arg(c, 1))));
pass!(get_material, GetMaterial, |c| (gp(arg(c, 1))));
pass!(set_light, SetLight, |c| (arg(c, 1), cp(arg(c, 2))));
pass!(get_light, GetLight, |c| (arg(c, 1), gp(arg(c, 2))));
pass!(light_enable, LightEnable, |c| (arg(c, 1), BOOL(arg(c, 2) as i32)));
pass!(get_light_enable, GetLightEnable, |c| (arg(c, 1), gp(arg(c, 2))));
pass!(set_clip_plane, SetClipPlane, |c| (arg(c, 1), cp(arg(c, 2))));
pass!(get_clip_plane, GetClipPlane, |c| (arg(c, 1), gp(arg(c, 2))));
pass!(set_render_state, SetRenderState, |c| (D3DRENDERSTATETYPE(arg(c, 1) as i32), arg(c, 2)));
pass!(get_render_state, GetRenderState, |c| (D3DRENDERSTATETYPE(arg(c, 1) as i32), gp(arg(c, 2))));
pass!(set_clip_status, SetClipStatus, |c| (cp(arg(c, 1))));
pass!(get_clip_status, GetClipStatus, |c| (gp(arg(c, 1))));
pass!(get_texture_stage_state, GetTextureStageState, |c| (arg(c, 1), D3DTEXTURESTAGESTATETYPE(arg(c, 2) as i32), gp(arg(c, 3))));
pass!(set_texture_stage_state, SetTextureStageState, |c| (arg(c, 1), D3DTEXTURESTAGESTATETYPE(arg(c, 2) as i32), arg(c, 3)));
pass!(get_sampler_state, GetSamplerState, |c| (arg(c, 1), D3DSAMPLERSTATETYPE(arg(c, 2) as i32), gp(arg(c, 3))));
pass!(set_sampler_state, SetSamplerState, |c| (arg(c, 1), D3DSAMPLERSTATETYPE(arg(c, 2) as i32), arg(c, 3)));
pass!(validate_device, ValidateDevice, |c| (gp(arg(c, 1))));
pass!(set_scissor_rect, SetScissorRect, |c| (cp(arg(c, 1))));
pass!(get_scissor_rect, GetScissorRect, |c| (gp(arg(c, 1))));
pass!(set_software_vertex_processing, SetSoftwareVertexProcessing, |c| (BOOL(arg(c, 1) as i32)));
pass!(set_npatch_mode, SetNPatchMode, |c| (f32::from_bits(arg(c, 1))));
pass!(set_fvf, SetFVF, |c| (arg(c, 1)));
pass!(get_fvf, GetFVF, |c| (gp(arg(c, 1))));

// ---- resources (`resources`) ----

fn create_texture(c: &mut Cpu) -> u32 {
    let (p, v) = dv(c);
    let a = |k| arg(c, k);
    let mut t: *mut c_void = std::ptr::null_mut();
    let r = unsafe { (v.CreateTexture)(p, a(1), a(2), a(3), a(4), D3DFORMAT(a(5)), D3DPOOL(a(6) as i32), &mut t, std::ptr::null_mut()) };
    resources::created(r, t, resources::TEXTURE, a(7), "CreateTexture")
}

fn create_vertex_buffer(c: &mut Cpu) -> u32 {
    let (p, v) = dv(c);
    let a = |k| arg(c, k);
    let mut b: *mut c_void = std::ptr::null_mut();
    let r = unsafe { (v.CreateVertexBuffer)(p, a(1), a(2), a(3), D3DPOOL(a(4) as i32), &mut b, std::ptr::null_mut()) };
    resources::created(r, b, resources::VERTEX_BUFFER, a(5), "CreateVertexBuffer")
}

fn create_index_buffer(c: &mut Cpu) -> u32 {
    let (p, v) = dv(c);
    let a = |k| arg(c, k);
    let mut b: *mut c_void = std::ptr::null_mut();
    let r = unsafe { (v.CreateIndexBuffer)(p, a(1), a(2), D3DFORMAT(a(3)), D3DPOOL(a(4) as i32), &mut b, std::ptr::null_mut()) };
    resources::created(r, b, resources::INDEX_BUFFER, a(5), "CreateIndexBuffer")
}

/// CreateQuery(type, out): a null out pointer asks only whether the type is supported.
fn create_query(c: &mut Cpu) -> u32 {
    let (p, v) = dv(c);
    let (ty, out) = (arg(c, 1), arg(c, 2));
    let mut q: *mut c_void = std::ptr::null_mut();
    let r = unsafe { (v.CreateQuery)(p, D3DQUERYTYPE(ty as i32), if out == 0 { std::ptr::null_mut() } else { &mut q }) };
    resources::created(r, q, resources::QUERY, out, "CreateQuery")
}

pass!(set_texture, SetTexture, |c| (arg(c, 1), resources::host_of(arg(c, 2))));
pass!(set_stream_source, SetStreamSource, |c| (arg(c, 1), resources::host_of(arg(c, 2)), arg(c, 3), arg(c, 4)));
pass!(set_indices, SetIndices, |c| (resources::host_of(arg(c, 1))));
// PALETTEENTRY: 4 bytes
pass!(set_palette_entries, SetPaletteEntries, |c| (arg(c, 1), cp(arg(c, 2))));
pass!(get_palette_entries, GetPaletteEntries, |c| (arg(c, 1), gp(arg(c, 2))));
pass!(set_current_texture_palette, SetCurrentTexturePalette, |c| (arg(c, 1)));
pass!(get_current_texture_palette, GetCurrentTexturePalette, |c| (gp(arg(c, 1))));

// SetCursorProperties(x hotspot, y hotspot, surface)
pass!(set_cursor_properties, SetCursorProperties, |c| (arg(c, 1), arg(c, 2), resources::host_of(arg(c, 3))));

/// A host interface the device hands out (`get` fills it) into the game's out pointer at
/// `out`; `wrap` finds or makes its guest object. Null out of D3D9 gives 0 and its result.
fn handed_out(c: &Cpu, out: u32, what: &str, wrap: impl FnOnce(*mut c_void) -> u32, get: impl FnOnce(*mut c_void, &'static IDirect3DDevice9_Vtbl, *mut *mut c_void) -> windows::core::HRESULT) -> u32 {
    let (p, v) = dv(c);
    let mut o: *mut c_void = std::ptr::null_mut();
    let r = get(p, v, &mut o);
    if r.is_err() || o.is_null() {
        if r.is_err() {
            log!("warn: {what} failed: {:#x}", r.0);
        }
        mem().wr32(out, 0);
        return hr(r);
    }
    mem().wr32(out, wrap(o));
    0
}

fn surface(h: *mut c_void) -> u32 {
    com::wrap(resources::SURFACE, h)
}

/// GetBackBuffer(swap chain, index, type, out).
fn get_back_buffer(c: &mut Cpu) -> u32 {
    let (sc, i, ty) = (arg(c, 1), arg(c, 2), arg(c, 3));
    handed_out(c, arg(c, 4), "GetBackBuffer", surface, |p, v, o| unsafe { (v.GetBackBuffer)(p, sc, i, D3DBACKBUFFER_TYPE(ty as i32), o) })
}

/// CreateCubeTexture(edge, levels, usage, format, pool, out, shared handle).
fn create_cube_texture(c: &mut Cpu) -> u32 {
    let (p, v) = dv(c);
    let a = |k| arg(c, k);
    let mut t: *mut c_void = std::ptr::null_mut();
    let r = unsafe { (v.CreateCubeTexture)(p, a(1), a(2), a(3), D3DFORMAT(a(4)), D3DPOOL(a(5) as i32), &mut t, std::ptr::null_mut()) };
    resources::created(r, t, resources::CUBE_TEXTURE, a(6), "CreateCubeTexture")
}

/// CreateRenderTarget(w, h, format, multisample, quality, lockable, out, shared handle).
fn create_render_target(c: &mut Cpu) -> u32 {
    let (p, v) = dv(c);
    let a = |k| arg(c, k);
    let mut s: *mut c_void = std::ptr::null_mut();
    let r = unsafe {
        (v.CreateRenderTarget)(p, a(1), a(2), D3DFORMAT(a(3)), D3DMULTISAMPLE_TYPE(a(4) as i32), a(5), BOOL(a(6) as i32), &mut s, std::ptr::null_mut())
    };
    resources::created(r, s, resources::SURFACE, a(7), "CreateRenderTarget")
}

/// CreateDepthStencilSurface(w, h, format, multisample, quality, discard, out, shared handle).
fn create_depth_stencil_surface(c: &mut Cpu) -> u32 {
    let (p, v) = dv(c);
    let a = |k| arg(c, k);
    let mut s: *mut c_void = std::ptr::null_mut();
    let r = unsafe {
        (v.CreateDepthStencilSurface)(p, a(1), a(2), D3DFORMAT(a(3)), D3DMULTISAMPLE_TYPE(a(4) as i32), a(5), BOOL(a(6) as i32), &mut s, std::ptr::null_mut())
    };
    resources::created(r, s, resources::SURFACE, a(7), "CreateDepthStencilSurface")
}

/// CreateOffscreenPlainSurface(w, h, format, pool, out, shared handle).
fn create_offscreen_plain_surface(c: &mut Cpu) -> u32 {
    let (p, v) = dv(c);
    let a = |k| arg(c, k);
    let mut s: *mut c_void = std::ptr::null_mut();
    let r = unsafe { (v.CreateOffscreenPlainSurface)(p, a(1), a(2), D3DFORMAT(a(3)), D3DPOOL(a(4) as i32), &mut s, std::ptr::null_mut()) };
    resources::created(r, s, resources::SURFACE, a(5), "CreateOffscreenPlainSurface")
}

use resources::host_of as h;

// Copies between surfaces and textures: the destination is never a buffer, so no kept guest
// copy goes stale (textures and surfaces are copied in at every lock).
pass!(update_surface, UpdateSurface, |c| (h(arg(c, 1)), cp(arg(c, 2)), h(arg(c, 3)), cp(arg(c, 4))));
pass!(update_texture, UpdateTexture, |c| (h(arg(c, 1)), h(arg(c, 2))));
pass!(get_render_target_data, GetRenderTargetData, |c| (h(arg(c, 1)), h(arg(c, 2))));
pass!(get_front_buffer_data, GetFrontBufferData, |c| (arg(c, 1), h(arg(c, 2))));
pass!(stretch_rect, StretchRect, |c| (h(arg(c, 1)), cp(arg(c, 2)), h(arg(c, 3)), cp(arg(c, 4)), D3DTEXTUREFILTERTYPE(arg(c, 5) as i32)));
pass!(color_fill, ColorFill, |c| (h(arg(c, 1)), cp(arg(c, 2)), arg(c, 3)));
pass!(set_render_target, SetRenderTarget, |c| (arg(c, 1), h(arg(c, 2))));
pass!(set_depth_stencil_surface, SetDepthStencilSurface, |c| (h(arg(c, 1))));

fn get_render_target(c: &mut Cpu) -> u32 {
    let i = arg(c, 1);
    handed_out(c, arg(c, 2), "GetRenderTarget", surface, |p, v, o| unsafe { (v.GetRenderTarget)(p, i, o) })
}

fn get_depth_stencil_surface(c: &mut Cpu) -> u32 {
    handed_out(c, arg(c, 1), "GetDepthStencilSurface", surface, |p, v, o| unsafe { (v.GetDepthStencilSurface)(p, o) })
}

// ---- state blocks ----

fn create_state_block(c: &mut Cpu) -> u32 {
    let (p, v) = dv(c);
    let mut b: *mut c_void = std::ptr::null_mut();
    let r = unsafe { (v.CreateStateBlock)(p, D3DSTATEBLOCKTYPE(arg(c, 1) as i32), &mut b) };
    resources::created(r, b, resources::STATE_BLOCK, arg(c, 2), "CreateStateBlock")
}

pass!(begin_state_block, BeginStateBlock, |c| ());

fn end_state_block(c: &mut Cpu) -> u32 {
    let (p, v) = dv(c);
    let mut b: *mut c_void = std::ptr::null_mut();
    let r = unsafe { (v.EndStateBlock)(p, &mut b) };
    resources::created(r, b, resources::STATE_BLOCK, arg(c, 1), "EndStateBlock")
}

fn get_texture(c: &mut Cpu) -> u32 {
    let stage = arg(c, 1);
    handed_out(c, arg(c, 2), "GetTexture", resources::wrap_base_texture, |p, v, o| unsafe { (v.GetTexture)(p, stage, o) })
}

// ---- drawing ----

pass!(draw_primitive, DrawPrimitive, |c| (D3DPRIMITIVETYPE(arg(c, 1) as i32), arg(c, 2), arg(c, 3)));
pass!(draw_indexed_primitive, DrawIndexedPrimitive, |c| (D3DPRIMITIVETYPE(arg(c, 1) as i32), arg(c, 2) as i32, arg(c, 3), arg(c, 4), arg(c, 5), arg(c, 6)));

/// Whether guest bytes [a, a + n) are committed memory D3D9 can read (the game's *UP arrays
/// are on its heap or stack, so this holds; a bad pointer is refused instead of faulting
/// inside d3d9.dll).
fn guest_readable(a: u32, n: u32) -> bool {
    use windows_sys::Win32::System::Memory::{MEM_COMMIT, MEMORY_BASIC_INFORMATION, VirtualQuery};
    let (mut at, end) = (a as u64, a as u64 + n as u64);
    while at < end {
        let mut mbi: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
        let p = unsafe { mem().base.add(at as usize) };
        if unsafe { VirtualQuery(p as _, &mut mbi, std::mem::size_of::<MEMORY_BASIC_INFORMATION>()) } == 0 || mbi.State != MEM_COMMIT {
            return false;
        }
        at = (mbi.BaseAddress as u64 - mem().base as u64) + mbi.RegionSize as u64;
    }
    true
}

/// A *UP draw's arrays not all in guest memory: logged, D3DERR_INVALIDCALL.
fn up_refused(what: &str, a: u32, n: u32) -> bool {
    if guest_readable(a, n) {
        return false;
    }
    log!("warn: {what}: {n} bytes at {a:#x} not in guest memory (D3DERR_INVALIDCALL)");
    true
}

/// DrawPrimitiveUP(type, count, vertices, stride): D3D9 reads the vertices where the game has
/// them (guest memory is host memory).
fn draw_primitive_up(c: &mut Cpu) -> u32 {
    let (p, v) = dv(c);
    let (ty, count, data, stride) = (arg(c, 1), arg(c, 2), arg(c, 3), arg(c, 4));
    if up_refused("DrawPrimitiveUP", data, resources::primitive_vertices(ty, count).saturating_mul(stride)) {
        return resources::D3DERR_INVALIDCALL;
    }
    hr(unsafe { (v.DrawPrimitiveUP)(p, D3DPRIMITIVETYPE(ty as i32), count, cp(data), stride) })
}

/// DrawIndexedPrimitiveUP(type, min index, vertex count, primitive count, indices, index
/// format, vertices, stride).
fn draw_indexed_primitive_up(c: &mut Cpu) -> u32 {
    let (p, v) = dv(c);
    let a = |k| arg(c, k);
    let (ty, min, nv, count, idx, fmt, data, stride) = (a(1), a(2), a(3), a(4), a(5), a(6), a(7), a(8));
    // D3DFMT_INDEX16 101, D3DFMT_INDEX32 102
    let index_bytes = resources::primitive_vertices(ty, count).saturating_mul(if fmt == 102 { 4 } else { 2 });
    if up_refused("DrawIndexedPrimitiveUP indices", idx, index_bytes)
        || up_refused("DrawIndexedPrimitiveUP vertices", data, min.saturating_add(nv).saturating_mul(stride))
    {
        return resources::D3DERR_INVALIDCALL;
    }
    hr(unsafe { (v.DrawIndexedPrimitiveUP)(p, D3DPRIMITIVETYPE(ty as i32), min, nv, count, cp(idx), D3DFORMAT(fmt), cp(data), stride) })
}

/// ProcessVertices(source start, destination index, count, destination buffer, declaration,
/// flags): the GPU writes the buffer, whose kept guest copy is stale then.
fn process_vertices(c: &mut Cpu) -> u32 {
    let (p, v) = dv(c);
    let a = |k| arg(c, k);
    resources::stale(a(4));
    hr(unsafe { (v.ProcessVertices)(p, a(1), a(2), a(3), h(a(4)), h(a(5)), a(6)) })
}

// ---- vertex declarations and shaders ----

/// CreateVertexDeclaration(elements, out): the array copied up to its end marker.
fn create_vertex_declaration(c: &mut Cpu) -> u32 {
    let (p, v) = dv(c);
    let elements = resources::vertex_elements(arg(c, 1));
    let mut d: *mut c_void = std::ptr::null_mut();
    let r = unsafe { (v.CreateVertexDeclaration)(p, elements.as_ptr(), &mut d) };
    resources::created(r, d, resources::VERTEX_DECLARATION, arg(c, 2), "CreateVertexDeclaration")
}

pass!(set_vertex_declaration, SetVertexDeclaration, |c| (h(arg(c, 1))));

fn get_vertex_declaration(c: &mut Cpu) -> u32 {
    handed_out(c, arg(c, 1), "GetVertexDeclaration", |o| com::wrap(resources::VERTEX_DECLARATION, o), |p, v, o| unsafe { (v.GetVertexDeclaration)(p, o) })
}

/// CreateVertexShader(bytecode, out): the game's bytecode (from its D3DX9) as it is; D3D9
/// reads it to its end token.
fn create_vertex_shader(c: &mut Cpu) -> u32 {
    let (p, v) = dv(c);
    let mut s: *mut c_void = std::ptr::null_mut();
    let r = unsafe { (v.CreateVertexShader)(p, cp(arg(c, 1)), &mut s) };
    resources::created(r, s, resources::VERTEX_SHADER, arg(c, 2), "CreateVertexShader")
}

fn create_pixel_shader(c: &mut Cpu) -> u32 {
    let (p, v) = dv(c);
    let mut s: *mut c_void = std::ptr::null_mut();
    let r = unsafe { (v.CreatePixelShader)(p, cp(arg(c, 1)), &mut s) };
    resources::created(r, s, resources::PIXEL_SHADER, arg(c, 2), "CreatePixelShader")
}

pass!(set_vertex_shader, SetVertexShader, |c| (h(arg(c, 1))));
pass!(set_pixel_shader, SetPixelShader, |c| (h(arg(c, 1))));

fn get_vertex_shader(c: &mut Cpu) -> u32 {
    handed_out(c, arg(c, 1), "GetVertexShader", |o| com::wrap(resources::VERTEX_SHADER, o), |p, v, o| unsafe { (v.GetVertexShader)(p, o) })
}

fn get_pixel_shader(c: &mut Cpu) -> u32 {
    handed_out(c, arg(c, 1), "GetPixelShader", |o| com::wrap(resources::PIXEL_SHADER, o), |p, v, o| unsafe { (v.GetPixelShader)(p, o) })
}

// constants (start register, data, count): floats, ints and BOOLs of 4 bytes, the same layout
pass!(set_vs_const_f, SetVertexShaderConstantF, |c| (arg(c, 1), cp(arg(c, 2)), arg(c, 3)));
pass!(get_vs_const_f, GetVertexShaderConstantF, |c| (arg(c, 1), gp(arg(c, 2)), arg(c, 3)));
pass!(set_vs_const_i, SetVertexShaderConstantI, |c| (arg(c, 1), cp(arg(c, 2)), arg(c, 3)));
pass!(get_vs_const_i, GetVertexShaderConstantI, |c| (arg(c, 1), gp(arg(c, 2)), arg(c, 3)));
pass!(set_vs_const_b, SetVertexShaderConstantB, |c| (arg(c, 1), cp(arg(c, 2)), arg(c, 3)));
pass!(get_vs_const_b, GetVertexShaderConstantB, |c| (arg(c, 1), gp(arg(c, 2)), arg(c, 3)));
pass!(set_ps_const_f, SetPixelShaderConstantF, |c| (arg(c, 1), cp(arg(c, 2)), arg(c, 3)));
pass!(get_ps_const_f, GetPixelShaderConstantF, |c| (arg(c, 1), gp(arg(c, 2)), arg(c, 3)));
pass!(set_ps_const_i, SetPixelShaderConstantI, |c| (arg(c, 1), cp(arg(c, 2)), arg(c, 3)));
pass!(get_ps_const_i, GetPixelShaderConstantI, |c| (arg(c, 1), gp(arg(c, 2)), arg(c, 3)));
pass!(set_ps_const_b, SetPixelShaderConstantB, |c| (arg(c, 1), cp(arg(c, 2)), arg(c, 3)));
pass!(get_ps_const_b, GetPixelShaderConstantB, |c| (arg(c, 1), gp(arg(c, 2)), arg(c, 3)));

// ---- streams ----

/// GetStreamSource(stream, out buffer, out offset, out stride).
fn get_stream_source(c: &mut Cpu) -> u32 {
    let (s, off, stride) = (arg(c, 1), arg(c, 3), arg(c, 4));
    handed_out(c, arg(c, 2), "GetStreamSource", |o| com::wrap(resources::VERTEX_BUFFER, o), |p, v, o| unsafe { (v.GetStreamSource)(p, s, o, gp(off), gp(stride)) })
}

pass!(set_stream_source_freq, SetStreamSourceFreq, |c| (arg(c, 1), arg(c, 2)));
pass!(get_stream_source_freq, GetStreamSourceFreq, |c| (arg(c, 1), gp(arg(c, 2))));

fn get_indices(c: &mut Cpu) -> u32 {
    handed_out(c, arg(c, 1), "GetIndices", |o| com::wrap(resources::INDEX_BUFFER, o), |p, v, o| unsafe { (v.GetIndices)(p, o) })
}

/// IDirect3DDevice9, in the SDK's order (119 methods).
pub static METHODS: &[Import] = &[
    ("IDirect3DDevice9::QueryInterface", 3, qi),
    ("IDirect3DDevice9::AddRef", 1, add_ref),
    ("IDirect3DDevice9::Release", 1, device_release),
    ("IDirect3DDevice9::TestCooperativeLevel", 1, test_cooperative_level),
    ("IDirect3DDevice9::GetAvailableTextureMem", 1, get_available_texture_mem),
    ("IDirect3DDevice9::EvictManagedResources", 1, evict_managed_resources),
    ("IDirect3DDevice9::GetDirect3D", 2, get_direct3d),
    ("IDirect3DDevice9::GetDeviceCaps", 2, get_device_caps),
    ("IDirect3DDevice9::GetDisplayMode", 3, get_display_mode),
    ("IDirect3DDevice9::GetCreationParameters", 2, get_creation_parameters),
    ("IDirect3DDevice9::SetCursorProperties", 4, set_cursor_properties),
    ("IDirect3DDevice9::SetCursorPosition", 4, set_cursor_position),
    ("IDirect3DDevice9::ShowCursor", 2, show_cursor),
    ("IDirect3DDevice9::CreateAdditionalSwapChain", 3, NI),
    ("IDirect3DDevice9::GetSwapChain", 3, NI),
    ("IDirect3DDevice9::GetNumberOfSwapChains", 1, get_number_of_swap_chains),
    ("IDirect3DDevice9::Reset", 2, reset),
    ("IDirect3DDevice9::Present", 5, present),
    ("IDirect3DDevice9::GetBackBuffer", 5, get_back_buffer),
    ("IDirect3DDevice9::GetRasterStatus", 3, get_raster_status),
    ("IDirect3DDevice9::SetDialogBoxMode", 2, set_dialog_box_mode),
    ("IDirect3DDevice9::SetGammaRamp", 4, set_gamma_ramp),
    ("IDirect3DDevice9::GetGammaRamp", 3, get_gamma_ramp),
    ("IDirect3DDevice9::CreateTexture", 9, create_texture),
    ("IDirect3DDevice9::CreateVolumeTexture", 10, NI),
    ("IDirect3DDevice9::CreateCubeTexture", 8, create_cube_texture),
    ("IDirect3DDevice9::CreateVertexBuffer", 7, create_vertex_buffer),
    ("IDirect3DDevice9::CreateIndexBuffer", 7, create_index_buffer),
    ("IDirect3DDevice9::CreateRenderTarget", 9, create_render_target),
    ("IDirect3DDevice9::CreateDepthStencilSurface", 9, create_depth_stencil_surface),
    ("IDirect3DDevice9::UpdateSurface", 5, update_surface),
    ("IDirect3DDevice9::UpdateTexture", 3, update_texture),
    ("IDirect3DDevice9::GetRenderTargetData", 3, get_render_target_data),
    ("IDirect3DDevice9::GetFrontBufferData", 3, get_front_buffer_data),
    ("IDirect3DDevice9::StretchRect", 6, stretch_rect),
    ("IDirect3DDevice9::ColorFill", 4, color_fill),
    ("IDirect3DDevice9::CreateOffscreenPlainSurface", 7, create_offscreen_plain_surface),
    ("IDirect3DDevice9::SetRenderTarget", 3, set_render_target),
    ("IDirect3DDevice9::GetRenderTarget", 3, get_render_target),
    ("IDirect3DDevice9::SetDepthStencilSurface", 2, set_depth_stencil_surface),
    ("IDirect3DDevice9::GetDepthStencilSurface", 2, get_depth_stencil_surface),
    ("IDirect3DDevice9::BeginScene", 1, begin_scene),
    ("IDirect3DDevice9::EndScene", 1, end_scene),
    ("IDirect3DDevice9::Clear", 7, clear),
    ("IDirect3DDevice9::SetTransform", 3, set_transform),
    ("IDirect3DDevice9::GetTransform", 3, get_transform),
    ("IDirect3DDevice9::MultiplyTransform", 3, multiply_transform),
    ("IDirect3DDevice9::SetViewport", 2, set_viewport),
    ("IDirect3DDevice9::GetViewport", 2, get_viewport),
    ("IDirect3DDevice9::SetMaterial", 2, set_material),
    ("IDirect3DDevice9::GetMaterial", 2, get_material),
    ("IDirect3DDevice9::SetLight", 3, set_light),
    ("IDirect3DDevice9::GetLight", 3, get_light),
    ("IDirect3DDevice9::LightEnable", 3, light_enable),
    ("IDirect3DDevice9::GetLightEnable", 3, get_light_enable),
    ("IDirect3DDevice9::SetClipPlane", 3, set_clip_plane),
    ("IDirect3DDevice9::GetClipPlane", 3, get_clip_plane),
    ("IDirect3DDevice9::SetRenderState", 3, set_render_state),
    ("IDirect3DDevice9::GetRenderState", 3, get_render_state),
    ("IDirect3DDevice9::CreateStateBlock", 3, create_state_block),
    ("IDirect3DDevice9::BeginStateBlock", 1, begin_state_block),
    ("IDirect3DDevice9::EndStateBlock", 2, end_state_block),
    ("IDirect3DDevice9::SetClipStatus", 2, set_clip_status),
    ("IDirect3DDevice9::GetClipStatus", 2, get_clip_status),
    ("IDirect3DDevice9::GetTexture", 3, get_texture),
    ("IDirect3DDevice9::SetTexture", 3, set_texture),
    ("IDirect3DDevice9::GetTextureStageState", 4, get_texture_stage_state),
    ("IDirect3DDevice9::SetTextureStageState", 4, set_texture_stage_state),
    ("IDirect3DDevice9::GetSamplerState", 4, get_sampler_state),
    ("IDirect3DDevice9::SetSamplerState", 4, set_sampler_state),
    ("IDirect3DDevice9::ValidateDevice", 2, validate_device),
    ("IDirect3DDevice9::SetPaletteEntries", 3, set_palette_entries),
    ("IDirect3DDevice9::GetPaletteEntries", 3, get_palette_entries),
    ("IDirect3DDevice9::SetCurrentTexturePalette", 2, set_current_texture_palette),
    ("IDirect3DDevice9::GetCurrentTexturePalette", 2, get_current_texture_palette),
    ("IDirect3DDevice9::SetScissorRect", 2, set_scissor_rect),
    ("IDirect3DDevice9::GetScissorRect", 2, get_scissor_rect),
    ("IDirect3DDevice9::SetSoftwareVertexProcessing", 2, set_software_vertex_processing),
    ("IDirect3DDevice9::GetSoftwareVertexProcessing", 1, get_software_vertex_processing),
    ("IDirect3DDevice9::SetNPatchMode", 2, set_npatch_mode),
    ("IDirect3DDevice9::GetNPatchMode", 1, NI),
    ("IDirect3DDevice9::DrawPrimitive", 4, draw_primitive),
    ("IDirect3DDevice9::DrawIndexedPrimitive", 7, draw_indexed_primitive),
    ("IDirect3DDevice9::DrawPrimitiveUP", 5, draw_primitive_up),
    ("IDirect3DDevice9::DrawIndexedPrimitiveUP", 9, draw_indexed_primitive_up),
    ("IDirect3DDevice9::ProcessVertices", 7, process_vertices),
    ("IDirect3DDevice9::CreateVertexDeclaration", 3, create_vertex_declaration),
    ("IDirect3DDevice9::SetVertexDeclaration", 2, set_vertex_declaration),
    ("IDirect3DDevice9::GetVertexDeclaration", 2, get_vertex_declaration),
    ("IDirect3DDevice9::SetFVF", 2, set_fvf),
    ("IDirect3DDevice9::GetFVF", 2, get_fvf),
    ("IDirect3DDevice9::CreateVertexShader", 3, create_vertex_shader),
    ("IDirect3DDevice9::SetVertexShader", 2, set_vertex_shader),
    ("IDirect3DDevice9::GetVertexShader", 2, get_vertex_shader),
    ("IDirect3DDevice9::SetVertexShaderConstantF", 4, set_vs_const_f),
    ("IDirect3DDevice9::GetVertexShaderConstantF", 4, get_vs_const_f),
    ("IDirect3DDevice9::SetVertexShaderConstantI", 4, set_vs_const_i),
    ("IDirect3DDevice9::GetVertexShaderConstantI", 4, get_vs_const_i),
    ("IDirect3DDevice9::SetVertexShaderConstantB", 4, set_vs_const_b),
    ("IDirect3DDevice9::GetVertexShaderConstantB", 4, get_vs_const_b),
    ("IDirect3DDevice9::SetStreamSource", 5, set_stream_source),
    ("IDirect3DDevice9::GetStreamSource", 5, get_stream_source),
    ("IDirect3DDevice9::SetStreamSourceFreq", 3, set_stream_source_freq),
    ("IDirect3DDevice9::GetStreamSourceFreq", 3, get_stream_source_freq),
    ("IDirect3DDevice9::SetIndices", 2, set_indices),
    ("IDirect3DDevice9::GetIndices", 2, get_indices),
    ("IDirect3DDevice9::CreatePixelShader", 3, create_pixel_shader),
    ("IDirect3DDevice9::SetPixelShader", 2, set_pixel_shader),
    ("IDirect3DDevice9::GetPixelShader", 2, get_pixel_shader),
    ("IDirect3DDevice9::SetPixelShaderConstantF", 4, set_ps_const_f),
    ("IDirect3DDevice9::GetPixelShaderConstantF", 4, get_ps_const_f),
    ("IDirect3DDevice9::SetPixelShaderConstantI", 4, set_ps_const_i),
    ("IDirect3DDevice9::GetPixelShaderConstantI", 4, get_ps_const_i),
    ("IDirect3DDevice9::SetPixelShaderConstantB", 4, set_ps_const_b),
    ("IDirect3DDevice9::GetPixelShaderConstantB", 4, get_ps_const_b),
    ("IDirect3DDevice9::DrawRectPatch", 4, NI),
    ("IDirect3DDevice9::DrawTriPatch", 4, NI),
    ("IDirect3DDevice9::DeletePatch", 2, NI),
    ("IDirect3DDevice9::CreateQuery", 3, create_query),
];
