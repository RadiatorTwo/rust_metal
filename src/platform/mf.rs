//! Media Foundation: sound files (the game's music is WMA, data\music\*.wma; WAV works too)
//! decoded to 16-bit PCM with the system's decoders through an `IMFSourceReader`. DirectShow's
//! filter graph (`dshow.rs`) plays music with it (`music.rs`).
//!
//! mfplat.dll and mfreadwrite.dll are loaded at run time (`api`), not imported: Windows "N"
//! editions without the Media Feature Pack lack them, and an import would keep gunmetal.exe from
//! starting at all. Media Foundation is started once (`MFStartup`). `available` asks whether
//! the DLLs are there and the system has a WMA decoder; `GUNMETAL_NO_MF=1` makes it say no.
//! Without one the game's music stays silent.
//!
//! Each thread that reads is put into COM's multithreaded apartment first. The game's own
//! threads never call in here: the music player decodes on a host thread of its own, and
//! `available` asks on one.

use super::log;
use std::ffi::c_void;
use std::path::Path;
use std::sync::OnceLock;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree};
use windows::Win32::System::Variant::VT_UI8;
use windows::core::{GUID, HRESULT, HSTRING, Interface, PCWSTR};

/// Decoded sound: interleaved 16-bit samples (`samples.len()` is frames times channels).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pcm {
    pub rate: u32,
    pub channels: u16,
    pub samples: Vec<i16>,
}

/// The Media Foundation functions we call, from the DLLs loaded at run time.
struct Api {
    startup: unsafe extern "system" fn(u32, u32) -> HRESULT,
    tenum_ex: unsafe extern "system" fn(GUID, u32, *const MFT_REGISTER_TYPE_INFO, *const MFT_REGISTER_TYPE_INFO, *mut *mut *mut c_void, *mut u32) -> HRESULT,
    create_media_type: unsafe extern "system" fn(*mut *mut c_void) -> HRESULT,
    create_source_reader_from_url: unsafe extern "system" fn(PCWSTR, *mut c_void, *mut *mut c_void) -> HRESULT,
}

/// The functions, or None when a DLL or a function is missing.
fn api() -> Option<&'static Api> {
    use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
    static API: OnceLock<Option<Api>> = OnceLock::new();
    API.get_or_init(|| unsafe {
        let load = |dll: &str| {
            let w: Vec<u16> = dll.encode_utf16().chain([0]).collect();
            let h = LoadLibraryW(w.as_ptr());
            (!h.is_null()).then_some(h)
        };
        let (plat, rw) = (load("mfplat.dll")?, load("mfreadwrite.dll")?);
        let f = |h, name: &[u8]| GetProcAddress(h, name.as_ptr());
        Some(Api {
            startup: std::mem::transmute(f(plat, b"MFStartup\0")?),
            tenum_ex: std::mem::transmute(f(plat, b"MFTEnumEx\0")?),
            create_media_type: std::mem::transmute(f(plat, b"MFCreateMediaType\0")?),
            create_source_reader_from_url: std::mem::transmute(f(rw, b"MFCreateSourceReaderFromURL\0")?),
        })
    })
    .as_ref()
}

/// COM on this thread (multithreaded apartment; an apartment it already has stays) and Media
/// Foundation in the process.
fn startup() -> Result<&'static Api, String> {
    static STARTED: OnceLock<Result<(), String>> = OnceLock::new();
    let api = api().ok_or("no Media Foundation (mfplat.dll, mfreadwrite.dll)")?;
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    STARTED.get_or_init(|| unsafe { (api.startup)(MF_VERSION, MFSTARTUP_NOSOCKET) }.ok().map_err(|e| format!("MFStartup: {e}"))).clone()?;
    Ok(api)
}

/// Whether music can be decoded: Media Foundation is there, starts and has a WMA decoder, and
/// `GUNMETAL_NO_MF` is not set. Asked once; the answer "no" is logged.
pub fn available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        let yes = std::env::var_os("GUNMETAL_NO_MF").is_none() && std::thread::spawn(has_wma_decoder).join().unwrap_or(false);
        if !yes {
            log!("no Media Foundation WMA decoder: music off");
        }
        yes
    })
}

fn has_wma_decoder() -> bool {
    let Ok(api) = startup() else { return false };
    let input = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Audio, guidSubtype: MFAudioFormat_WMAudioV8 };
    let (mut list, mut n) = (std::ptr::null_mut::<*mut c_void>(), 0u32);
    let found = unsafe { (api.tenum_ex)(MFT_CATEGORY_AUDIO_DECODER, MFT_ENUM_FLAG_ALL.0 as u32, &input, std::ptr::null(), &mut list, &mut n) };
    if !list.is_null() {
        unsafe {
            for k in 0..n as usize {
                // each IMFActivate's reference goes
                drop(IMFActivate::from_raw(*list.add(k)));
            }
            CoTaskMemFree(Some(list as *const _));
        }
    }
    found.is_ok() && n > 0
}

/// A COM out parameter filled by `f` as interface `T`.
unsafe fn make<T: Interface>(f: impl FnOnce(*mut *mut c_void) -> HRESULT) -> windows::core::Result<T> {
    let mut p = std::ptr::null_mut();
    f(&mut p).ok()?;
    if p.is_null() { Err(windows::core::Error::from_hresult(HRESULT(0x8000_4003u32 as i32))) } else { Ok(unsafe { T::from_raw(p) }) }
}

/// A sound file being decoded: its first audio stream as 16-bit PCM, piece by piece.
pub struct Reader {
    reader: IMFSourceReader,
    pub rate: u32,
    pub channels: u16,
    /// in 100 ns units (0 when the file does not say)
    pub duration: u64,
}

const AUDIO: u32 = MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32;

impl Reader {
    pub fn open(path: &Path) -> Result<Reader, String> {
        let api = startup()?;
        let full = std::path::absolute(path).map_err(|e| format!("{}: {e}", path.display()))?;
        // Media Foundation takes no "\\?\" paths (the game folder may be one)
        let full = full.to_string_lossy();
        let url = match full.strip_prefix(r"\\?\UNC\") {
            Some(share) => format!(r"\\{share}"),
            None => full.trim_start_matches(r"\\?\").to_string(),
        };
        let err = |what: &str, e: windows::core::Error| format!("{}: {what}: {e}", path.display());
        unsafe {
            let url = HSTRING::from(url);
            let reader: IMFSourceReader =
                make(|p| (api.create_source_reader_from_url)(PCWSTR(url.as_ptr()), std::ptr::null_mut(), p)).map_err(|e| err("open", e))?;
            reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false).map_err(|e| err("streams", e))?;
            reader.SetStreamSelection(AUDIO, true).map_err(|e| err("no audio stream", e))?;
            let want: IMFMediaType = make(|p| (api.create_media_type)(p)).map_err(|e| err("media type", e))?;
            want.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio).map_err(|e| err("media type", e))?;
            want.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM).map_err(|e| err("media type", e))?;
            want.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16).map_err(|e| err("media type", e))?;
            reader.SetCurrentMediaType(AUDIO, None, &want).map_err(|e| err("no decoder to PCM", e))?;
            let got = reader.GetCurrentMediaType(AUDIO).map_err(|e| err("media type", e))?;
            let rate = got.GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND).map_err(|e| err("rate", e))?;
            let channels = got.GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS).map_err(|e| err("channels", e))? as u16;
            let duration = reader
                .GetPresentationAttribute(MF_SOURCE_READER_MEDIASOURCE.0 as u32, &MF_PD_DURATION)
                .ok()
                .and_then(|v: PROPVARIANT| (v.Anonymous.Anonymous.vt == VT_UI8).then(|| v.Anonymous.Anonymous.Anonymous.uhVal))
                .unwrap_or(0);
            Ok(Reader { reader, rate, channels, duration })
        }
    }

    /// The next decoded piece, appended to `out`; false at the end of the file.
    pub fn read(&mut self, out: &mut Vec<i16>) -> Result<bool, String> {
        loop {
            let (mut flags, mut sample) = (0u32, None);
            unsafe { self.reader.ReadSample(AUDIO, 0, None, Some(&mut flags), None, Some(&mut sample)) }
                .map_err(|e| format!("ReadSample: {e}"))?;
            if let Some(s) = sample {
                unsafe {
                    let buf = s.ConvertToContiguousBuffer().map_err(|e| format!("sample: {e}"))?;
                    let (mut p, mut len) = (std::ptr::null_mut::<u8>(), 0u32);
                    buf.Lock(&mut p, None, Some(&mut len)).map_err(|e| format!("sample: {e}"))?;
                    let bytes = std::slice::from_raw_parts(p, len as usize);
                    out.extend(bytes.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])));
                    let _ = buf.Unlock();
                }
                return Ok(true);
            }
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                return Ok(false);
            }
            // a gap or a format note without data: read on
        }
    }

    /// Go to time `t` (100 ns units).
    pub fn seek(&mut self, t: u64) -> Result<(), String> {
        let v = PROPVARIANT::from(t as i64);
        unsafe { self.reader.SetCurrentPosition(&GUID::zeroed(), &v) }.map_err(|e| format!("seek: {e}"))
    }
}

/// A whole sound file decoded.
pub fn decode(path: &Path) -> Result<Pcm, String> {
    let mut r = Reader::open(path)?;
    let mut samples = Vec::new();
    while r.read(&mut samples)? {}
    Ok(Pcm { rate: r.rate, channels: r.channels, samples })
}
