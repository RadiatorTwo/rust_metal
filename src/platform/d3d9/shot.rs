//! `GUNMETAL_SHOT=file.png`: the back buffer is written there before Present (the picture the
//! game is about to show), at the first Present and then at most every `GUNMETAL_SHOT_MS`
//! (500 ms if not set). A `{t}` in the name is replaced by the time since start in ms, so
//! every picture is kept. For checks without looking (as rustschweine's HOGS_SHOT).

use crate::platform::{kernel32, lock, log};
use std::ffi::c_void;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use windows::Win32::Graphics::Direct3D9::*;
use windows::core::Interface;

fn path() -> Option<&'static String> {
    static P: OnceLock<Option<String>> = OnceLock::new();
    P.get_or_init(|| std::env::var("GUNMETAL_SHOT").ok()).as_ref()
}

fn every() -> Duration {
    static MS: OnceLock<u64> = OnceLock::new();
    Duration::from_millis(*MS.get_or_init(|| std::env::var("GUNMETAL_SHOT_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(500)))
}

static LAST: Mutex<Option<Instant>> = Mutex::new(None);

/// A picture asked for once (`request`).
static REQUESTED: Mutex<Option<String>> = Mutex::new(None);

/// Write the next picture to `file` (a mark of `GUNMETAL_RECORD`).
pub fn request(file: String) {
    *lock(&REQUESTED) = Some(file);
}

/// Before Present on the host device `dev`: write the back buffer (`back`, the game's own on
/// 9Ex; else the swap chain's) if a picture is due.
pub fn before_present(dev: *mut c_void, back: Option<*mut c_void>) {
    let requested = lock(&REQUESTED).take();
    let Some(path) = requested.as_ref().or(path()) else { return };
    if requested.is_none() {
        let mut last = lock(&LAST);
        if last.is_some_and(|t| t.elapsed() < every()) {
            return;
        }
        *last = Some(Instant::now());
    }
    let Some(d) = (unsafe { IDirect3DDevice9::from_raw_borrowed(&dev) }) else {
        return;
    };
    let back = back.and_then(|b| unsafe { IDirect3DSurface9::from_raw_borrowed(&b) }.cloned());
    match unsafe { back_buffer(d, back) } {
        Ok((px, w, h)) => {
            let t = kernel32::start_time().elapsed().as_millis();
            let file = path.replace("{t}", &format!("{t:06}"));
            if requested.is_some() {
                log!("picture written to {file}");
            }
            if let Err(e) = std::fs::write(&file, png(&px, w, h)) {
                log!("warn: GUNMETAL_SHOT {file}: {e}");
            }
        }
        Err(e) => log!("warn: GUNMETAL_SHOT: cannot read the back buffer: {e}"),
    }
}

/// The back buffer as 0RGB pixels: copied to system memory with GetRenderTargetData (through a
/// render target without multisampling when the back buffer has it).
unsafe fn back_buffer(d: &IDirect3DDevice9, back: Option<IDirect3DSurface9>) -> windows::core::Result<(Vec<u32>, u32, u32)> {
    unsafe {
        let mut bb = match back {
            Some(b) => b,
            None => d.GetBackBuffer(0, 0, D3DBACKBUFFER_TYPE_MONO)?,
        };
        let mut desc = D3DSURFACE_DESC::default();
        bb.GetDesc(&mut desc)?;
        let (w, h) = (desc.Width, desc.Height);
        if desc.MultiSampleType != D3DMULTISAMPLE_NONE {
            let mut rt = None;
            d.CreateRenderTarget(w, h, desc.Format, D3DMULTISAMPLE_NONE, 0, false, &mut rt, std::ptr::null_mut())?;
            let rt = rt.unwrap();
            d.StretchRect(&bb, std::ptr::null(), &rt, std::ptr::null(), D3DTEXF_NONE)?;
            bb = rt;
        }
        let mut sys = None;
        d.CreateOffscreenPlainSurface(w, h, desc.Format, D3DPOOL_SYSTEMMEM, &mut sys, std::ptr::null_mut())?;
        let sys = sys.unwrap();
        d.GetRenderTargetData(&bb, &sys)?;
        let mut lr = D3DLOCKED_RECT::default();
        sys.LockRect(&mut lr, std::ptr::null(), D3DLOCK_READONLY as u32)?;
        let mut px = Vec::with_capacity((w * h) as usize);
        for y in 0..h {
            let row = (lr.pBits as *const u8).add(y as usize * lr.Pitch as usize);
            for x in 0..w as usize {
                px.push(match desc.Format {
                    D3DFMT_R5G6B5 => {
                        let v = (row.add(2 * x) as *const u16).read_unaligned() as u32;
                        ((v >> 11) * 255 / 31) << 16 | ((v >> 5 & 63) * 255 / 63) << 8 | (v & 31) * 255 / 31
                    }
                    D3DFMT_X1R5G5B5 | D3DFMT_A1R5G5B5 => {
                        let v = (row.add(2 * x) as *const u16).read_unaligned() as u32;
                        ((v >> 10 & 31) * 255 / 31) << 16 | ((v >> 5 & 31) * 255 / 31) << 8 | (v & 31) * 255 / 31
                    }
                    _ => (row.add(4 * x) as *const u32).read_unaligned() & 0xff_ffff,
                });
            }
        }
        sys.UnlockRect()?;
        Ok((px, w, h))
    }
}

/// An uncompressed PNG of 0RGB pixels.
fn png(pixels: &[u32], w: u32, h: u32) -> Vec<u8> {
    fn crc(data: &[u8]) -> u32 {
        let mut c = !0u32;
        for &b in data {
            c ^= b as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
            }
        }
        !c
    }
    fn chunk(out: &mut Vec<u8>, kind: &[u8], data: &[u8]) {
        out.extend((data.len() as u32).to_be_bytes());
        let start = out.len();
        out.extend(kind);
        out.extend(data);
        let c = crc(&out[start..]);
        out.extend(c.to_be_bytes());
    }
    let mut raw = Vec::with_capacity(((w * 3 + 1) * h) as usize);
    for row in pixels.chunks(w as usize).take(h as usize) {
        raw.push(0);
        for p in row {
            raw.extend([(p >> 16) as u8, (p >> 8) as u8, *p as u8]);
        }
    }
    // zlib stream of stored deflate blocks
    let mut z = vec![0x78, 0x01];
    let blocks: Vec<&[u8]> = raw.chunks(0xffff).collect();
    for (k, b) in blocks.iter().enumerate() {
        z.push((k + 1 == blocks.len()) as u8);
        z.extend((b.len() as u16).to_le_bytes());
        z.extend((!(b.len() as u16)).to_le_bytes());
        z.extend(*b);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &x in &raw {
        a = (a + x as u32) % 65521;
        b = (b + a) % 65521;
    }
    z.extend((b << 16 | a).to_be_bytes());
    let mut out = vec![0x89, b'P', b'N', b'G', 13, 10, 0x1a, 10];
    let mut ihdr = Vec::new();
    ihdr.extend(w.to_be_bytes());
    ihdr.extend(h.to_be_bytes());
    ihdr.extend([8, 2, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}
