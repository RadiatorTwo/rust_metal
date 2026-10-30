//! The music player behind a filter graph (`dshow.rs`) that was given a sound file: one host
//! thread per file decodes it with Media Foundation (`mf::Reader`) and plays it through the
//! system's waveOut (winmm) device.
//!
//! Why waveOut and not DirectSound: the game's DirectSound objects are its own (`dsound.rs`
//! passes them through); music needs an output of its own that no guest code touches, and
//! waveOut is the simplest one: no window for a cooperative level, no play cursor to chase,
//! four 50 ms blocks queued and refilled when done. The volume (IBasicAudio, hundredths of a
//! dB) is applied to the samples as they are queued, so it takes effect within 200 ms; waveOut's
//! own volume would be the whole process's on current Windows.
//!
//! The thread never touches guest memory or guest CPU state. The game's side (IMediaControl,
//! IMediaSeeking, IBasicAudio on the game's threads) only changes the shared `State`; at the end
//! of the file the thread marks EC_COMPLETE as waiting and sets the graph's guest event through
//! `kernel32::set_event_state` (thread-safe), which wakes the game's music thread; the graph
//! hands the event out at its next GetEvent (`take_complete`). The game loops a track by
//! seeking to 0 when it gets EC_COMPLETE.

use super::{kernel32, lock, log, mf};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;
use windows::Win32::Media::Audio::*;
use windows::core::PSTR;

/// What the game asked for and what the thread reports, shared between them.
#[derive(Default)]
struct State {
    running: bool,
    seek: Option<u64>,
    /// IBasicAudio volume: hundredths of a dB, -10000 (silent) to 0
    volume: i32,
    /// the graph's guest event handle (0: none yet)
    event: u32,
    /// EC_COMPLETE waits for the graph to hand it out
    complete: bool,
    /// played to the end (until the next seek)
    ended: bool,
    /// 100 ns units
    position: u64,
    duration: u64,
    quit: bool,
}

#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    wake: Condvar,
}

/// A sound file playing (or ready to) on its own host thread.
pub struct Player {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl Player {
    /// Start the thread for `path` (stopped at the start); `event`: the graph's guest event
    /// handle if it has one already.
    pub fn open(path: PathBuf, event: u32, volume: i32) -> Player {
        let shared = Arc::new(Shared::default());
        {
            let mut s = lock(&shared.state);
            s.event = event;
            s.volume = volume;
        }
        let sh = shared.clone();
        let thread = std::thread::Builder::new().name("music".into()).spawn(move || play(&path, &sh)).ok();
        Player { shared, thread }
    }

    fn change(&self, f: impl FnOnce(&mut State)) {
        f(&mut lock(&self.shared.state));
        self.shared.wake.notify_all();
    }

    fn get<T>(&self, f: impl FnOnce(&State) -> T) -> T {
        f(&lock(&self.shared.state))
    }

    pub fn run(&self) {
        self.change(|s| s.running = true);
    }

    /// Pause and Stop: both keep the position.
    pub fn pause(&self) {
        self.change(|s| s.running = false);
    }

    /// Go to `t` (100 ns units); a waiting EC_COMPLETE is gone.
    pub fn seek(&self, t: u64) {
        self.change(|s| {
            s.seek = Some(t);
            s.position = t;
            s.complete = false;
            s.ended = false;
        });
    }

    pub fn set_volume(&self, v: i32) {
        self.change(|s| s.volume = v);
    }

    pub fn set_event(&self, h: u32) {
        self.change(|s| s.event = h);
    }

    /// Whether EC_COMPLETE waits; it is handed out once.
    pub fn take_complete(&self) -> bool {
        let mut s = lock(&self.shared.state);
        let c = std::mem::take(&mut s.complete);
        s.ended |= c;
        c
    }

    /// Whether EC_COMPLETE waits, without handing it out.
    pub fn complete_waits(&self) -> bool {
        self.get(|s| s.complete)
    }

    pub fn ended(&self) -> bool {
        self.get(|s| s.ended || s.complete)
    }

    /// (position, duration) in 100 ns units.
    pub fn times(&self) -> (u64, u64) {
        self.get(|s| (s.position, s.duration))
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.change(|s| s.quit = true);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Volume in hundredths of a dB as a factor.
pub fn gain(volume: i32) -> f32 {
    if volume <= -10000 { 0.0 } else { 10f32.powf(volume.min(0) as f32 / 2000.0) }
}

/// The player thread: decode and queue while running, until `quit`.
fn play(path: &Path, shared: &Shared) {
    let mut media = match mf::Reader::open(path).and_then(|r| Output::open(r.rate, r.channels).map(|o| (r, o))) {
        Ok((r, o)) => {
            log!("music: {}: {} Hz, {} channels, {:.1} s", path.display(), r.rate, r.channels, r.duration as f64 / 1e7);
            lock(&shared.state).duration = r.duration;
            Some((r, o))
        }
        Err(e) => {
            log!("warn: music: {e} (silent)");
            None
        }
    };
    let mut pending: Vec<i16> = Vec::new();
    let (mut at_end, mut base, mut played) = (false, 0u64, 0u64);
    let mut s = lock(&shared.state);
    while !s.quit {
        let (running, g) = (s.running, gain(s.volume));
        if let Some(t) = s.seek.take() {
            if let Some((r, o)) = &mut media {
                o.reset();
                if let Err(e) = r.seek(t) {
                    log!("warn: music: {}: {e}", path.display());
                }
            }
            pending.clear();
            (at_end, base, played) = (false, t, 0);
        }
        // decoding and queueing without the lock: the game's calls do not wait for it
        drop(s);
        let idle = match &mut media {
            Some((r, o)) => {
                o.set_paused(!running);
                if running {
                    while !at_end && let Some(k) = o.free_block() {
                        let want = o.block_frames * r.channels as usize;
                        while pending.len() < want && !at_end {
                            match r.read(&mut pending) {
                                Ok(more) => at_end = !more,
                                Err(e) => {
                                    log!("warn: music: {}: {e}", path.display());
                                    at_end = true;
                                }
                            }
                        }
                        let n = want.min(pending.len());
                        if n == 0 {
                            break;
                        }
                        o.queue(k, pending.drain(..n), g);
                    }
                }
                played = o.played_frames();
                o.idle()
            }
            None => {
                at_end = true;
                true
            }
        };
        s = lock(&shared.state);
        if let Some((r, _)) = &media
            && s.seek.is_none()
        {
            s.position = base + played * 10_000_000 / r.rate as u64;
        }
        if running && at_end && idle && !s.ended && !s.complete && s.seek.is_none() {
            s.complete = true;
            if s.event != 0 {
                kernel32::set_event_state(s.event, true);
            }
        }
        s = shared.wake.wait_timeout(s, Duration::from_millis(10)).unwrap_or_else(|e| e.into_inner()).0;
    }
}

const BLOCKS: usize = 4;

/// A waveOut device with `BLOCKS` blocks of 50 ms each.
struct Output {
    h: HWAVEOUT,
    headers: Box<[WAVEHDR; BLOCKS]>,
    data: Vec<Vec<i16>>,
    queued: [bool; BLOCKS],
    block_frames: usize,
    channels: usize,
    paused: bool,
    /// frames of the blocks that have played since the last reset
    done_frames: u64,
}

impl Output {
    fn open(rate: u32, channels: u16) -> Result<Output, String> {
        let align = 2 * channels;
        let wfx = WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_PCM as u16,
            nChannels: channels,
            nSamplesPerSec: rate,
            nAvgBytesPerSec: rate * align as u32,
            nBlockAlign: align,
            wBitsPerSample: 16,
            cbSize: 0,
        };
        let mut h = HWAVEOUT::default();
        let r = unsafe { waveOutOpen(Some(&mut h), WAVE_MAPPER, &wfx, None, None, CALLBACK_NULL) };
        if r != 0 {
            return Err(format!("waveOutOpen({rate} Hz, {channels} channels): error {r}"));
        }
        let block_frames = (rate / 20).max(1) as usize;
        let mut o = Output {
            h,
            headers: Box::new([WAVEHDR::default(); BLOCKS]),
            data: vec![vec![0i16; block_frames * channels as usize]; BLOCKS],
            queued: [false; BLOCKS],
            block_frames,
            channels: channels as usize,
            paused: false,
            done_frames: 0,
        };
        for k in 0..BLOCKS {
            o.headers[k].lpData = PSTR(o.data[k].as_mut_ptr().cast());
            o.headers[k].dwBufferLength = 0;
        }
        Ok(o)
    }

    const HDR: u32 = std::mem::size_of::<WAVEHDR>() as u32;

    /// A block that is not queued (blocks that have played are counted and freed).
    fn free_block(&mut self) -> Option<usize> {
        self.collect();
        self.queued.iter().position(|q| !q)
    }

    fn collect(&mut self) {
        for k in 0..BLOCKS {
            if self.queued[k] && self.headers[k].dwFlags & WHDR_DONE != 0 {
                unsafe { waveOutUnprepareHeader(self.h, &mut self.headers[k], Self::HDR) };
                self.queued[k] = false;
                self.done_frames += (self.headers[k].dwBufferLength as usize / 2 / self.channels) as u64;
            }
        }
    }

    /// Queue `samples` (at most a block) in block `k`, scaled by `gain`.
    fn queue(&mut self, k: usize, samples: impl Iterator<Item = i16>, gain: f32) {
        let mut n = 0;
        for (d, s) in self.data[k].iter_mut().zip(samples) {
            *d = (s as f32 * gain) as i16;
            n += 1;
        }
        let h = &mut self.headers[k];
        h.dwBufferLength = 2 * n as u32;
        h.dwFlags = 0;
        unsafe {
            waveOutPrepareHeader(self.h, h, Self::HDR);
            waveOutWrite(self.h, h, Self::HDR);
        }
        self.queued[k] = true;
    }

    fn set_paused(&mut self, p: bool) {
        if p != self.paused {
            unsafe {
                if p {
                    waveOutPause(self.h);
                } else {
                    waveOutRestart(self.h);
                }
            }
            self.paused = p;
        }
    }

    /// Drop what is queued; the count of played frames starts again.
    fn reset(&mut self) {
        unsafe { waveOutReset(self.h) };
        self.collect();
        self.done_frames = 0;
    }

    fn played_frames(&mut self) -> u64 {
        self.collect();
        self.done_frames
    }

    /// Nothing queued.
    fn idle(&mut self) -> bool {
        self.collect();
        !self.queued.iter().any(|&q| q)
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        unsafe {
            waveOutReset(self.h);
            self.collect();
            waveOutClose(self.h);
        }
    }
}
