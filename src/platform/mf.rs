//! Media Foundation: sound files (the game's music is WMA, data\music\*.wma; WAV works too)
//! decoded to 16-bit PCM with the system's decoders through an `IMFSourceReader`. DirectShow's
//! filter graph (`dshow.rs`) plays music with it (`music.rs`).
//!
//! Media Foundation is started once (`MFStartup`). `available` asks whether the system has a
//! WMA decoder (Windows "N" editions have none without the Media Feature Pack);
//! `GUNMETAL_NO_MF=1` makes it say no. Without one the game's music stays silent.
//!
//! Each thread that reads is put into COM's multithreaded apartment first. The game's own
//! threads never call in here: the music player decodes on a host thread of its own, and
//! `available` asks on one.

use super::log;
use std::path::Path;
use std::sync::OnceLock;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree};
use windows::core::{GUID, HSTRING};

/// Decoded sound: interleaved 16-bit samples (`samples.len()` is frames times channels).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pcm {
    pub rate: u32,
    pub channels: u16,
    pub samples: Vec<i16>,
}

/// COM on this thread (multithreaded apartment; an apartment it already has stays) and Media
/// Foundation in the process.
fn startup() -> Result<(), String> {
    static STARTED: OnceLock<Result<(), String>> = OnceLock::new();
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    STARTED.get_or_init(|| unsafe { MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET) }.map_err(|e| format!("MFStartup: {e}"))).clone()
}

/// Whether music can be decoded: Media Foundation starts and has a WMA decoder, and
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
    if startup().is_err() {
        return false;
    }
    let input = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Audio, guidSubtype: MFAudioFormat_WMAudioV8 };
    let (mut list, mut n) = (std::ptr::null_mut::<Option<IMFActivate>>(), 0u32);
    let found = unsafe { MFTEnumEx(MFT_CATEGORY_AUDIO_DECODER, MFT_ENUM_FLAG_ALL, Some(&input), None, &mut list, &mut n) };
    if !list.is_null() {
        unsafe {
            for k in 0..n as usize {
                std::ptr::drop_in_place(list.add(k));
            }
            CoTaskMemFree(Some(list as *const _));
        }
    }
    found.is_ok() && n > 0
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
        startup()?;
        let full = std::path::absolute(path).map_err(|e| format!("{}: {e}", path.display()))?;
        // Media Foundation takes no "\\?\" paths (the game folder may be one)
        let full = full.to_string_lossy();
        let url = match full.strip_prefix(r"\\?\UNC\") {
            Some(share) => format!(r"\\{share}"),
            None => full.trim_start_matches(r"\\?\").to_string(),
        };
        let err = |what: &str, e: windows::core::Error| format!("{}: {what}: {e}", path.display());
        unsafe {
            let reader = MFCreateSourceReaderFromURL(&HSTRING::from(url), None).map_err(|e| err("open", e))?;
            reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false).map_err(|e| err("streams", e))?;
            reader.SetStreamSelection(AUDIO, true).map_err(|e| err("no audio stream", e))?;
            let want = MFCreateMediaType().map_err(|e| err("media type", e))?;
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
                .and_then(|v| u64::try_from(&v).ok())
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
