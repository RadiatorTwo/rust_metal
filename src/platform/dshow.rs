//! DirectShow: our own filter graph (CLSID_FilterGraph) and Video Mixing Renderer 9. Music
//! plays (sound files go to a player of their own, `music.rs`, decoded by Media Foundation,
//! `mf.rs`); videos are not played yet: they start and end at once, and the game goes on as if
//! each one had ended.
//!
//! The game's music (music_* in data/symbols.txt; class vtable 0x5e5ac8): music_start
//! (0x467c90) makes a "done" event and a worker thread (0x4690b0) whose open callback is
//! music_open (0x467eb0), on that thread: CoCreateInstance(CLSID_FilterGraph), QueryInterface
//! IMediaControl (+0x118), IBasicAudio (+0x11c), IMediaEventEx (+0x120), IMediaSeeking
//! (+0x124), GetEventHandle (+0x12c), which the worker waits for, RenderFile of the full path
//! ("C:\Spiele\Gun Metal\data\music\new_frontend.wma" in the front end), put_Volume
//! (music_volume_db 0x468390: 2000 log10 of the setting, clamped to -10000..0) and Run. When
//! the event is set the worker calls music_on_event (0x468140): GetEvent(timeout 0) until there
//! are none; EC_COMPLETE seeks to 0 with IMediaSeeking::SetPositions (looping, +9) or sets the
//! done event; FreeEventParams each. music_pause (0x467d90) and music_resume (0x467dd0) are
//! IMediaControl Pause and Run, music_set_volume (0x467e50) put_Volume; music_close
//! (0x468280) releases the interfaces.
//!
//! So a graph given a sound file (RenderFile, AddSourceFilter) gets a player: Run, Pause, Stop
//! drive it, put_Volume is its volume, IMediaSeeking and IMediaPosition its position and
//! duration (from the decoder), and at the end of the file it posts EC_COMPLETE through the
//! graph's event (`Player::take_complete`). Without Media Foundation's WMA decoder
//! (`mf::available`) sound files take the videos' way: silent, ended at once.
//!
//! The game's videos (video_start 0x4174a0: the intro "data\frontend\cgplayed_01.wmv", the
//! attract loop "attract_ps.wmv"; "_h.wmv" instead of ".wmv" for high quality) take one of two
//! players, chosen by video_use_vmr9 (0x535410: the setting Extras/VideoMethod, read in
//! game_start into proj_data +0x2e4: 1 texture, 2 VMR9, otherwise VMR9 on Windows NT 5 and
//! later; the game's data has 1, so the videotexture player runs; the VMR9 one is implemented
//! from its code but not yet seen running):
//! - videoplayer9 (vtable 0x5ecb60; vmr9_open 0x5836c0): CoCreateInstance(CLSID_FilterGraph,
//!   IID_IGraphBuilder), CoCreateInstance(CLSID_VideoMixingRenderer9, IID_IBaseFilter),
//!   IVMRFilterConfig9 SetRenderingMode(renderless), SetNumberOfStreams(2), its own surface
//!   allocator (0x583fa0, vtable 0x5ecbec; vmr9_set_allocator 0x583cb0:
//!   IVMRSurfaceAllocatorNotify9::AdviseSurfaceAllocator, then the allocator's AdviseNotify
//!   calls SetD3DDevice), AddFilter(VMR9, "Video Mixing Renderer 9"), QueryInterface
//!   IMediaControl, IMediaPosition, IBasicAudio, RenderFile(path), Run;
//! - videotexture (vtable 0x5ecc14; vtex_open 0x5866d0): the graph, AddSourceFilter(path,
//!   "SOURCE"), its own "TEXTURERENDERER" filter (0x586b50) added with AddFilter, Render of each
//!   output pin of the source (EnumPins), QueryInterface IMediaControl, IMediaPosition,
//!   IMediaEvent, IBasicAudio, get_Duration, Run.
//! Each frame (video_frame 0x417820) the game polls the player (videotexture:
//! vtex_poll_events 0x586290, IMediaEvent::GetEvent; EC_COMPLETE ends the video or, when it
//! loops, seeks to 0), draws it, sleeps 25 ms and asks whether it ended (vmr9_ended 0x583b70,
//! vtex_ended 0x586360): IMediaPosition duration minus current position below 1 ms.
//!
//! So a video graph reports duration 0 and position 0, and Run posts EC_COMPLETE (1) to its event
//! queue (IMediaEvent; the event handle from GetEventHandle is set while events wait). The
//! source filter has no pins, so nothing is rendered; the VMR9 never asks its allocator for
//! surfaces. The graph holds a reference to each filter added (as DirectShow, which the game's
//! own renderer relies on: its FindPin and the pin's Release go through the filter's count)
//! and tells it through JoinFilterGraph; filters are not run.
//!
//! Each of our objects is a set of guest interface blocks (vtable, object index) sharing one
//! reference count in a host table.

use super::kernel32::{self, Obj};
use super::ole32::{guid, guid_at, guid_string};
use super::{Import, arg, com_new, free, guest_call, lock, log, mem, method_not_implemented as nimp, wstr};
#[cfg(windows)]
use super::music::Player;
use crate::rt::Cpu;
use std::collections::VecDeque;
use std::sync::Mutex;

pub const S_OK: u32 = 0;
pub const S_FALSE: u32 = 1;
const E_NOTIMPL: u32 = 0x8000_4001;
const E_NOINTERFACE: u32 = 0x8000_4002;
const E_POINTER: u32 = 0x8000_4003;
pub const E_ABORT: u32 = 0x8000_4004;
const E_INVALIDARG: u32 = 0x8007_0057;
const VFW_E_NOT_FOUND: u32 = 0x8004_0216;
/// EC_COMPLETE: all data of the graph has been rendered.
pub const EC_COMPLETE: u32 = 1;

const QUARTZ: [u8; 8] = [0xb0, 0x3a, 0x00, 0x20, 0xaf, 0x0b, 0xa7, 0x70];
const fn quartz(d1: u32) -> [u8; 16] {
    guid(d1, 0x0ad4, 0x11ce, QUARTZ)
}
pub const CLSID_FILTER_GRAPH: [u8; 16] = guid(0xe436_ebb3, 0x524f, 0x11ce, [0x9f, 0x53, 0x00, 0x20, 0xaf, 0x0b, 0xa7, 0x70]);
pub const CLSID_VMR9: [u8; 16] = guid(0x51b4_abf3, 0x748f, 0x4e3b, [0xa2, 0x76, 0xc8, 0x28, 0x33, 0x0e, 0x92, 0x6a]);
const CLSID_ASYNC_READER: [u8; 16] = guid(0xe436_ebb5, 0x524f, 0x11ce, [0x9f, 0x53, 0x00, 0x20, 0xaf, 0x0b, 0xa7, 0x70]);
pub const IID_IUNKNOWN: [u8; 16] = guid(0, 0, 0, [0xc0, 0, 0, 0, 0, 0, 0, 0x46]);
const IID_IPERSIST: [u8; 16] = guid(0x0000_010c, 0, 0, [0xc0, 0, 0, 0, 0, 0, 0, 0x46]);
pub const IID_IFILTERGRAPH: [u8; 16] = quartz(0x56a8_689f);
pub const IID_IGRAPHBUILDER: [u8; 16] = quartz(0x56a8_68a9);
pub const IID_IMEDIACONTROL: [u8; 16] = quartz(0x56a8_68b1);
pub const IID_IMEDIAPOSITION: [u8; 16] = quartz(0x56a8_68b2);
pub const IID_IBASICAUDIO: [u8; 16] = quartz(0x56a8_68b3);
pub const IID_IMEDIAEVENT: [u8; 16] = quartz(0x56a8_68b6);
pub const IID_IMEDIAEVENTEX: [u8; 16] = quartz(0x56a8_68c0);
pub const IID_IMEDIASEEKING: [u8; 16] = guid(0x36b7_3880, 0xc2c8, 0x11cf, [0x8b, 0x46, 0x00, 0x80, 0x5f, 0x6c, 0xef, 0x60]);
pub const IID_IBASEFILTER: [u8; 16] = quartz(0x56a8_6895);
const IID_IMEDIAFILTER: [u8; 16] = quartz(0x56a8_6899);
pub const IID_IENUMPINS: [u8; 16] = quartz(0x56a8_6892);
const IID_IVMRFILTERCONFIG9: [u8; 16] = guid(0x5a80_4648, 0x4f66, 0x4867, [0x9c, 0x43, 0x4f, 0x5c, 0x82, 0x2c, 0xf1, 0xb8]);
const IID_IVMRSURFACEALLOCATORNOTIFY9: [u8; 16] = guid(0xdca3_f5df, 0xbb3a, 0x4d03, [0xbd, 0x81, 0x84, 0x61, 0x4b, 0xfb, 0xfa, 0x0c]);
const TIME_FORMAT_MEDIA_TIME: [u8; 16] = guid(0x7b78_5574, 0x8c82, 0x11cf, [0xbc, 0x0c, 0x00, 0xaa, 0x00, 0xac, 0x74, 0xf6]);

// ---- the graph's state -----------------------------------------------------------------------

/// A graph's event queue (IMediaEvent): (code, param1, param2) in order, and a manual-reset
/// event handle (made when the game asks for it) that is set while events wait.
#[derive(Default)]
pub struct Events {
    queue: VecDeque<[u32; 3]>,
    handle: u32,
}

impl Events {
    pub fn post(&mut self, code: u32, p1: u32, p2: u32) {
        self.queue.push_back([code, p1, p2]);
        if self.handle != 0 {
            kernel32::set_event_state(self.handle, true);
        }
    }

    /// The next event, if any.
    pub fn get(&mut self) -> Option<[u32; 3]> {
        let e = self.queue.pop_front();
        if self.queue.is_empty() && self.handle != 0 {
            kernel32::set_event_state(self.handle, false);
        }
        e
    }

    /// The event handle (one per graph).
    pub fn handle(&mut self) -> u32 {
        if self.handle == 0 {
            self.handle = kernel32::objects().add(Obj::Event { manual: true, set: !self.queue.is_empty() });
        }
        self.handle
    }
}

/// State_Stopped, State_Paused, State_Running
pub const STOPPED: u32 = 0;
pub const PAUSED: u32 = 1;
pub const RUNNING: u32 = 2;

/// A filter graph: the filters added (guest interface pointers, one reference each), the
/// files it was given, its state, events, volume and balance (IBasicAudio, hundredths of dB),
/// and the player of its sound file.
#[derive(Default)]
pub struct Graph {
    pub filters: Vec<u32>,
    pub files: Vec<String>,
    pub state: u32,
    pub events: Events,
    pub volume: i32,
    pub balance: i32,
    music: Option<Player>,
}

/// Without Windows there is no music: no player is ever made.
#[cfg(not(windows))]
enum Player {}
#[cfg(not(windows))]
#[rustfmt::skip]
impl Player {
    fn run(&self) { match *self {} }
    fn pause(&self) { match *self {} }
    fn seek(&self, _: u64) { match *self {} }
    fn set_volume(&self, _: i32) { match *self {} }
    fn set_event(&self, _: u32) { match *self {} }
    fn take_complete(&self) -> bool { match *self {} }
    fn complete_waits(&self) -> bool { match *self {} }
    fn ended(&self) -> bool { match *self {} }
    fn times(&self) -> (u64, u64) { match *self {} }
}

/// Whether a file is sound only.
pub fn is_audio(path: &str) -> bool {
    let p = path.to_ascii_lowercase();
    [".wma", ".wav", ".mp3"].iter().any(|e| p.ends_with(e))
}

/// A player for the sound file `path` (a guest path), if music can be played.
#[cfg(windows)]
fn open_music(path: &str, event: u32, volume: i32) -> Option<Player> {
    super::mf::available().then(|| Player::open(super::host_path(path), event, volume))
}

#[cfg(not(windows))]
fn open_music(_: &str, _: u32, _: i32) -> Option<Player> {
    None
}

fn note_file(g: &mut Graph, path: String) {
    if is_audio(&path) {
        g.music = open_music(&path, g.events.handle, g.volume);
        if g.music.is_some() {
            log!("dshow: {path}: music");
        } else {
            log!("dshow: {path}: sound is not played (silent, ends at once)");
        }
    } else {
        log!("dshow: {path}: video is skipped (ends at once)");
    }
    g.files.push(path);
}

// ---- objects with several interfaces ---------------------------------------------------------

enum Kind {
    Graph(Graph),
    /// the VMR9: its surface allocator (a guest object, one reference) and its settings
    Vmr9 { allocator: u32, mode: u32, streams: u32 },
    /// a source filter without pins
    Source,
    EnumPins,
}

struct Object {
    refs: u32,
    /// IID -> guest interface block; the first block also answers IUnknown
    ifaces: Vec<([u8; 16], u32)>,
    kind: Kind,
}

static OBJECTS: Mutex<Vec<Option<Object>>> = Mutex::new(Vec::new());

/// A new object with one reference; `ifaces`: per guest interface block, its IIDs and methods.
/// Returns its first interface.
fn new_object(kind: Kind, ifaces: &[(&[[u8; 16]], &'static [Import])]) -> u32 {
    let blocks: Vec<u32> = ifaces.iter().map(|i| com_new(i.1, 4)).collect();
    let mut list = vec![(IID_IUNKNOWN, blocks[0])];
    for (i, &b) in ifaces.iter().zip(&blocks) {
        list.extend(i.0.iter().map(|&iid| (iid, b)));
    }
    let mut t = lock(&OBJECTS);
    let o = Object { refs: 1, ifaces: list, kind };
    let k = match t.iter().position(Option::is_none) {
        Some(k) => {
            t[k] = Some(o);
            k
        }
        None => {
            t.push(Some(o));
            t.len() - 1
        }
    };
    for &b in &blocks {
        mem().wr32(b + 4, k as u32);
    }
    blocks[0]
}

/// The object index behind interface `this`, if it is one of ours.
fn ours(this: u32) -> Option<usize> {
    if this == 0 {
        return None;
    }
    let k = mem().rd32(this + 4) as usize;
    let t = lock(&OBJECTS);
    t.get(k)?.as_ref()?.ifaces.iter().any(|i| i.1 == this).then_some(k)
}

fn with<T>(this: u32, f: impl FnOnce(&mut Object) -> T) -> T {
    let k = mem().rd32(this + 4) as usize;
    f(lock(&OBJECTS)[k].as_mut().expect("DirectShow call on a released object"))
}

fn with_graph<T>(this: u32, f: impl FnOnce(&mut Graph) -> T) -> T {
    with(this, |o| match &mut o.kind {
        Kind::Graph(g) => f(g),
        _ => panic!("not a filter graph"),
    })
}

/// The interface `iid` of the object behind `this`, without a new reference.
fn iface(this: u32, iid: &[u8; 16]) -> Option<u32> {
    with(this, |o| o.ifaces.iter().find(|i| &i.0 == iid).map(|i| i.1))
}

/// The object behind `this` for interface `iid` (a new reference) into `out`.
fn query(this: u32, iid: [u8; 16], out: u32) -> u32 {
    if out == 0 {
        return E_POINTER;
    }
    match iface(this, &iid) {
        Some(p) => {
            with(this, |o| o.refs += 1);
            mem().wr32(out, p);
            S_OK
        }
        None => {
            log!("dshow: QueryInterface({}) of an object that does not have it", guid_string(&iid));
            mem().wr32(out, 0);
            E_NOINTERFACE
        }
    }
}

/// CoCreateInstance: a new object, its interface `iid` into `out`.
fn create(kind: Kind, ifaces: &[(&[[u8; 16]], &'static [Import])], iid: u32, out: u32) -> u32 {
    let o = new_object(kind, ifaces);
    let hr = query(o, guid_at(iid), out);
    // the new object's own reference goes; what `query` gave stays
    release_ours(None, o);
    hr
}

/// Call method `k` of a guest COM object.
fn call_method(c: &mut Cpu, obj: u32, k: u32, args: &[u32]) -> u32 {
    let m = mem();
    let f = m.rd32(m.rd32(obj) + 4 * k);
    let mut a = vec![obj];
    a.extend_from_slice(args);
    guest_call(c, f, &a)
}

fn add_ref_any(c: &mut Cpu, obj: u32) {
    match ours(obj) {
        Some(k) => lock(&OBJECTS)[k].as_mut().unwrap().refs += 1,
        None => {
            call_method(c, obj, 1, &[]);
        }
    }
}

fn release_any(c: &mut Cpu, obj: u32) {
    match ours(obj) {
        Some(_) => {
            release_ours(Some(c), obj);
        }
        None => {
            call_method(c, obj, 2, &[]);
        }
    }
}

/// Release one reference of our object behind `this`; at 0 it goes, with the references it
/// holds (released through `c`; an object holding guest references is never released
/// without it). Returns the count left.
fn release_ours(c: Option<&mut Cpu>, this: u32) -> u32 {
    let k = mem().rd32(this + 4) as usize;
    let gone = {
        let mut t = lock(&OBJECTS);
        let o = t[k].as_mut().expect("DirectShow object released twice");
        o.refs -= 1;
        if o.refs != 0 {
            return o.refs;
        }
        t[k].take().unwrap()
    };
    let mut blocks: Vec<u32> = gone.ifaces.iter().map(|i| i.1).collect();
    blocks.dedup();
    for b in blocks {
        free(b);
    }
    let held: Vec<u32> = match gone.kind {
        Kind::Graph(mut g) => {
            // the player's thread ends before the event it sets goes
            drop(g.music.take());
            if g.events.handle != 0 {
                kernel32::objects().remove(g.events.handle);
            }
            g.filters
        }
        Kind::Vmr9 { allocator, .. } => vec![allocator],
        Kind::Source | Kind::EnumPins => Vec::new(),
    };
    let held: Vec<u32> = held.into_iter().filter(|&f| f != 0).collect();
    if !held.is_empty() {
        let c = c.expect("a DirectShow object with references released without a CPU");
        for f in held {
            if ours(f).is_none() {
                // IBaseFilter::JoinFilterGraph(NULL, NULL) as the graph leaves it (for an
                // allocator this slot is not called: only filters are in a graph's list)
                if is_filter_list(&gone.ifaces) {
                    call_method(c, f, 13, &[0, 0]);
                }
            }
            release_any(c, f);
        }
    }
    0
}

fn is_filter_list(ifaces: &[([u8; 16], u32)]) -> bool {
    ifaces.iter().any(|i| i.0 == IID_IGRAPHBUILDER)
}

// ---- IUnknown of all our objects --------------------------------------------------------------

fn qi(c: &mut Cpu) -> u32 {
    query(arg(c, 0), guid_at(arg(c, 1)), arg(c, 2))
}

fn add_ref(c: &mut Cpu) -> u32 {
    with(arg(c, 0), |o| {
        o.refs += 1;
        o.refs
    })
}

fn release(c: &mut Cpu) -> u32 {
    let this = arg(c, 0);
    release_ours(Some(c), this)
}

fn ok(_: &mut Cpu) -> u32 {
    S_OK
}

fn not_impl(_: &mut Cpu) -> u32 {
    E_NOTIMPL
}

/// Write a dword to an out parameter (null allowed).
fn put32(p: u32, v: u32) -> u32 {
    if p == 0 {
        return E_POINTER;
    }
    mem().wr32(p, v);
    S_OK
}

/// Write 8 bytes (a double or a LONGLONG) to an out parameter (null allowed).
fn put64(p: u32, v: [u8; 8]) -> u32 {
    if p == 0 {
        return E_POINTER;
    }
    mem().slice(p, 8).copy_from_slice(&v);
    S_OK
}

// ---- the filter graph ---------------------------------------------------------------------------

/// CoCreateInstance(CLSID_FilterGraph).
pub fn create_graph(iid: u32, out: u32) -> u32 {
    create(
        Kind::Graph(Graph::default()),
        &[
            (&[IID_IFILTERGRAPH, IID_IGRAPHBUILDER], GRAPH_BUILDER),
            (&[IID_IMEDIACONTROL], MEDIA_CONTROL),
            (&[IID_IMEDIAEVENT, IID_IMEDIAEVENTEX], MEDIA_EVENT),
            (&[IID_IMEDIAPOSITION], MEDIA_POSITION),
            (&[IID_IMEDIASEEKING], MEDIA_SEEKING),
            (&[IID_IBASICAUDIO], BASIC_AUDIO),
        ],
        iid,
        out,
    )
}

/// The graph of interface `this` as IFilterGraph (what filters are told they joined).
fn graph_iface(this: u32) -> u32 {
    iface(this, &IID_IFILTERGRAPH).unwrap()
}

/// AddFilter(filter, name): one reference, and the filter is told it joined.
fn add_filter(c: &mut Cpu) -> u32 {
    let (this, f, name) = (arg(c, 0), arg(c, 1), arg(c, 2));
    if f == 0 {
        return E_POINTER;
    }
    add_ref_any(c, f);
    with_graph(this, |g| g.filters.push(f));
    if ours(f).is_none() {
        let hr = call_method(c, f, 13, &[graph_iface(this), name]);
        if hr as i32 >= 0 {
            return S_OK;
        }
        with_graph(this, |g| g.filters.retain(|&x| x != f));
        release_any(c, f);
        return hr;
    }
    S_OK
}

fn remove_filter(c: &mut Cpu) -> u32 {
    let (this, f) = (arg(c, 0), arg(c, 1));
    let had = with_graph(this, |g| {
        let n = g.filters.len();
        g.filters.retain(|&x| x != f);
        n != g.filters.len()
    });
    if !had {
        return VFW_E_NOT_FOUND;
    }
    if ours(f).is_none() {
        call_method(c, f, 13, &[0, 0]);
    }
    release_any(c, f);
    S_OK
}

/// RenderFile(file, playlist)
fn render_file(c: &mut Cpu) -> u32 {
    let (this, file) = (arg(c, 0), wstr(arg(c, 1)));
    with_graph(this, |g| note_file(g, file));
    S_OK
}

/// AddSourceFilter(file, name, out): a source filter without pins, in the graph.
fn add_source_filter(c: &mut Cpu) -> u32 {
    let (this, file, out) = (arg(c, 0), wstr(arg(c, 1)), arg(c, 3));
    if out == 0 {
        return E_POINTER;
    }
    let f = new_object(Kind::Source, &[(&[IID_IPERSIST, IID_IMEDIAFILTER, IID_IBASEFILTER], FILTER)]);
    // the caller's reference and the graph's
    with(f, |o| o.refs += 1);
    with_graph(this, |g| {
        note_file(g, file);
        g.filters.push(f);
    });
    mem().wr32(out, f);
    S_OK
}

pub static GRAPH_BUILDER: &[Import] = &[
    ("IGraphBuilder::QueryInterface", 3, qi),
    ("IGraphBuilder::AddRef", 1, add_ref),
    ("IGraphBuilder::Release", 1, release),
    ("IGraphBuilder::AddFilter", 3, add_filter),
    ("IGraphBuilder::RemoveFilter", 2, remove_filter),
    ("IGraphBuilder::EnumFilters", 2, nimp),
    ("IGraphBuilder::FindFilterByName", 3, nimp),
    ("IGraphBuilder::ConnectDirect", 4, ok),
    ("IGraphBuilder::Reconnect", 2, ok),
    ("IGraphBuilder::Disconnect", 2, ok),
    ("IGraphBuilder::SetDefaultSyncSource", 1, ok),
    ("IGraphBuilder::Connect", 3, ok),
    ("IGraphBuilder::Render", 2, ok),
    ("IGraphBuilder::RenderFile", 3, render_file),
    ("IGraphBuilder::AddSourceFilter", 4, add_source_filter),
    ("IGraphBuilder::SetLogFile", 2, ok),
    ("IGraphBuilder::Abort", 1, ok),
    ("IGraphBuilder::ShouldOperationContinue", 1, ok),
];

// ---- IMediaControl ----------------------------------------------------------------------------

/// Run: music plays; anything else ends at once (EC_COMPLETE) when the graph was not running.
fn run(c: &mut Cpu) -> u32 {
    with_graph(arg(c, 0), |g| {
        if let Some(p) = &g.music {
            p.run();
        } else if g.state != RUNNING {
            g.events.post(EC_COMPLETE, S_OK, 0);
        }
        g.state = RUNNING;
    });
    S_OK
}

/// Pause and Stop: music halts where it is.
fn halt(g: &mut Graph, state: u32) {
    if let Some(p) = &g.music {
        p.pause();
    }
    g.state = state;
}

fn pause(c: &mut Cpu) -> u32 {
    with_graph(arg(c, 0), |g| halt(g, PAUSED));
    S_OK
}

fn stop(c: &mut Cpu) -> u32 {
    with_graph(arg(c, 0), |g| halt(g, STOPPED));
    S_OK
}

/// GetState(timeout, out)
fn get_state(c: &mut Cpu) -> u32 {
    let s = with_graph(arg(c, 0), |g| g.state);
    put32(arg(c, 2), s)
}

/// IMediaControl::RenderFile(BSTR)
fn control_render_file(c: &mut Cpu) -> u32 {
    let (this, file) = (arg(c, 0), wstr(arg(c, 1)));
    with_graph(this, |g| note_file(g, file));
    S_OK
}

pub static MEDIA_CONTROL: &[Import] = &[
    ("IMediaControl::QueryInterface", 3, qi),
    ("IMediaControl::AddRef", 1, add_ref),
    ("IMediaControl::Release", 1, release),
    ("IMediaControl::GetTypeInfoCount", 2, not_impl),
    ("IMediaControl::GetTypeInfo", 4, not_impl),
    ("IMediaControl::GetIDsOfNames", 6, not_impl),
    ("IMediaControl::Invoke", 9, not_impl),
    ("IMediaControl::Run", 1, run),
    ("IMediaControl::Pause", 1, pause),
    ("IMediaControl::Stop", 1, stop),
    ("IMediaControl::GetState", 3, get_state),
    ("IMediaControl::RenderFile", 2, control_render_file),
    ("IMediaControl::AddSourceFilter", 3, nimp),
    ("IMediaControl::get_FilterCollection", 2, nimp),
    ("IMediaControl::get_RegFilterCollection", 2, nimp),
    ("IMediaControl::StopWhenReady", 1, stop),
];

// ---- IMediaEventEx ----------------------------------------------------------------------------

fn get_event_handle(c: &mut Cpu) -> u32 {
    let h = with_graph(arg(c, 0), |g| {
        let h = g.events.handle();
        if let Some(p) = &g.music {
            p.set_event(h);
        }
        h
    });
    put32(arg(c, 1), h)
}

/// GetEvent(code, param1, param2, timeout): the next event (the player's EC_COMPLETE joins the
/// queue first); none: E_ABORT at once (the game waits for the handle, not here).
fn get_event(c: &mut Cpu) -> u32 {
    let e = with_graph(arg(c, 0), |g| {
        if g.music.as_ref().is_some_and(Player::take_complete) {
            g.events.post(EC_COMPLETE, S_OK, 0);
        }
        let e = g.events.get();
        // the player may have finished in between: then its event stays set
        if g.music.as_ref().is_some_and(Player::complete_waits) {
            kernel32::set_event_state(g.events.handle(), true);
        }
        e
    });
    let [code, p1, p2] = e.unwrap_or([0; 3]);
    for (k, v) in [code, p1, p2].into_iter().enumerate() {
        if arg(c, 1 + k as u32) != 0 {
            mem().wr32(arg(c, 1 + k as u32), v);
        }
    }
    if e.is_some() { S_OK } else { E_ABORT }
}

/// WaitForCompletion(timeout, code): complete once it ran (music: once it played to the end).
fn wait_for_completion(c: &mut Cpu) -> u32 {
    let ran = with_graph(arg(c, 0), |g| g.state == RUNNING && g.music.as_ref().is_none_or(Player::ended));
    put32(arg(c, 2), if ran { EC_COMPLETE } else { 0 });
    if ran { S_OK } else { E_ABORT }
}

pub static MEDIA_EVENT: &[Import] = &[
    ("IMediaEventEx::QueryInterface", 3, qi),
    ("IMediaEventEx::AddRef", 1, add_ref),
    ("IMediaEventEx::Release", 1, release),
    ("IMediaEventEx::GetTypeInfoCount", 2, not_impl),
    ("IMediaEventEx::GetTypeInfo", 4, not_impl),
    ("IMediaEventEx::GetIDsOfNames", 6, not_impl),
    ("IMediaEventEx::Invoke", 9, not_impl),
    ("IMediaEventEx::GetEventHandle", 2, get_event_handle),
    ("IMediaEventEx::GetEvent", 5, get_event),
    ("IMediaEventEx::WaitForCompletion", 3, wait_for_completion),
    ("IMediaEventEx::CancelDefaultHandling", 2, ok),
    ("IMediaEventEx::RestoreDefaultHandling", 2, ok),
    ("IMediaEventEx::FreeEventParams", 4, ok),
    ("IMediaEventEx::SetNotifyWindow", 4, nimp),
    ("IMediaEventEx::SetNotifyFlags", 2, ok),
    ("IMediaEventEx::GetNotifyFlags", 2, nimp),
];

// ---- IMediaPosition: seconds; a video is at its end, which is 0 -------------------------------

/// (position, duration) of the graph's music in 100 ns units; (0, 0) without.
fn times(this: u32) -> (u64, u64) {
    with_graph(this, |g| g.music.as_ref().map_or((0, 0), Player::times))
}

fn seek(this: u32, t: u64) {
    with_graph(this, |g| {
        if let Some(p) = &g.music {
            p.seek(t);
        }
    });
}

fn seconds(t: u64) -> [u8; 8] {
    (t as f64 / 1e7).to_le_bytes()
}

fn zero_f64(c: &mut Cpu) -> u32 {
    put64(arg(c, 1), 0f64.to_le_bytes())
}

fn duration_f64(c: &mut Cpu) -> u32 {
    put64(arg(c, 1), seconds(times(arg(c, 0)).1))
}

fn position_f64(c: &mut Cpu) -> u32 {
    put64(arg(c, 1), seconds(times(arg(c, 0)).0))
}

/// put_CurrentPosition(double: two stack slots)
fn put_position(c: &mut Cpu) -> u32 {
    let s = f64::from_bits(arg(c, 1) as u64 | (arg(c, 2) as u64) << 32);
    seek(arg(c, 0), (s.max(0.0) * 1e7) as u64);
    S_OK
}

fn one_f64(c: &mut Cpu) -> u32 {
    put64(arg(c, 1), 1f64.to_le_bytes())
}

fn oa_true(c: &mut Cpu) -> u32 {
    put32(arg(c, 1), u32::MAX)
}

pub static MEDIA_POSITION: &[Import] = &[
    ("IMediaPosition::QueryInterface", 3, qi),
    ("IMediaPosition::AddRef", 1, add_ref),
    ("IMediaPosition::Release", 1, release),
    ("IMediaPosition::GetTypeInfoCount", 2, not_impl),
    ("IMediaPosition::GetTypeInfo", 4, not_impl),
    ("IMediaPosition::GetIDsOfNames", 6, not_impl),
    ("IMediaPosition::Invoke", 9, not_impl),
    ("IMediaPosition::get_Duration", 2, duration_f64),
    ("IMediaPosition::put_CurrentPosition", 3, put_position),
    ("IMediaPosition::get_CurrentPosition", 2, position_f64),
    ("IMediaPosition::get_StopTime", 2, duration_f64),
    ("IMediaPosition::put_StopTime", 3, ok),
    ("IMediaPosition::get_PrerollTime", 2, zero_f64),
    ("IMediaPosition::put_PrerollTime", 3, ok),
    ("IMediaPosition::put_Rate", 3, ok),
    ("IMediaPosition::get_Rate", 2, one_f64),
    ("IMediaPosition::CanSeekForward", 2, oa_true),
    ("IMediaPosition::CanSeekBackward", 2, oa_true),
];

// ---- IMediaSeeking: media time (100 ns); a video is at its end, which is 0 --------------------

/// GetCapabilities: absolute, forwards, backwards, current and stop position, duration.
fn seeking_caps(c: &mut Cpu) -> u32 {
    put32(arg(c, 1), 0x11f)
}

fn check_caps(c: &mut Cpu) -> u32 {
    let p = arg(c, 1);
    if p == 0 {
        return E_POINTER;
    }
    let want = mem().rd32(p);
    mem().wr32(p, want & 0x11f);
    if want & !0x11f == 0 { S_OK } else { S_FALSE }
}

/// IsFormatSupported, IsUsingTimeFormat(format): only media time.
fn is_media_time(c: &mut Cpu) -> u32 {
    if arg(c, 1) != 0 && guid_at(arg(c, 1)) == TIME_FORMAT_MEDIA_TIME { S_OK } else { S_FALSE }
}

fn set_time_format(c: &mut Cpu) -> u32 {
    if arg(c, 1) != 0 && guid_at(arg(c, 1)) == TIME_FORMAT_MEDIA_TIME { S_OK } else { E_INVALIDARG }
}

fn get_time_format(c: &mut Cpu) -> u32 {
    let p = arg(c, 1);
    if p == 0 {
        return E_POINTER;
    }
    mem().slice(p, 16).copy_from_slice(&TIME_FORMAT_MEDIA_TIME);
    S_OK
}

fn zero_i64(c: &mut Cpu) -> u32 {
    put64(arg(c, 1), [0; 8])
}

fn duration_i64(c: &mut Cpu) -> u32 {
    put64(arg(c, 1), times(arg(c, 0)).1.to_le_bytes())
}

fn position_i64(c: &mut Cpu) -> u32 {
    put64(arg(c, 1), times(arg(c, 0)).0.to_le_bytes())
}

/// Two out parameters: `first` and the duration.
fn first_and_duration(c: &mut Cpu, first: u64) -> u32 {
    let (_, d) = times(arg(c, 0));
    for (k, v) in [(1, first), (2, d)] {
        if arg(c, k) != 0 {
            put64(arg(c, k), v.to_le_bytes());
        }
    }
    S_OK
}

/// GetPositions(current, stop): the position and the end.
fn get_positions(c: &mut Cpu) -> u32 {
    let p = times(arg(c, 0)).0;
    first_and_duration(c, p)
}

/// GetAvailable(earliest, latest): all of it.
fn get_available(c: &mut Cpu) -> u32 {
    first_and_duration(c, 0)
}

/// SetPositions(current, current flags, stop, stop flags): the current position, absolute (1)
/// or relative (2) to where it is; the stop position stays the end.
fn set_positions(c: &mut Cpu) -> u32 {
    let (this, p, flags) = (arg(c, 0), arg(c, 1), arg(c, 2));
    if p == 0 {
        return S_OK;
    }
    let v = i64::from_le_bytes(mem().slice(p, 8).try_into().unwrap());
    let t = match flags & 3 {
        1 => v,
        2 => times(this).0 as i64 + v,
        _ => return S_OK,
    };
    seek(this, t.max(0) as u64);
    S_OK
}

/// ConvertTimeFormat(target, target format, source (8 bytes), source format): media time only.
fn convert_time_format(c: &mut Cpu) -> u32 {
    let (out, lo, hi) = (arg(c, 1), arg(c, 3), arg(c, 4));
    let mut v = [0u8; 8];
    v[..4].copy_from_slice(&lo.to_le_bytes());
    v[4..].copy_from_slice(&hi.to_le_bytes());
    put64(out, v)
}

pub static MEDIA_SEEKING: &[Import] = &[
    ("IMediaSeeking::QueryInterface", 3, qi),
    ("IMediaSeeking::AddRef", 1, add_ref),
    ("IMediaSeeking::Release", 1, release),
    ("IMediaSeeking::GetCapabilities", 2, seeking_caps),
    ("IMediaSeeking::CheckCapabilities", 2, check_caps),
    ("IMediaSeeking::IsFormatSupported", 2, is_media_time),
    ("IMediaSeeking::QueryPreferredFormat", 2, get_time_format),
    ("IMediaSeeking::GetTimeFormat", 2, get_time_format),
    ("IMediaSeeking::IsUsingTimeFormat", 2, is_media_time),
    ("IMediaSeeking::SetTimeFormat", 2, set_time_format),
    ("IMediaSeeking::GetDuration", 2, duration_i64),
    ("IMediaSeeking::GetStopPosition", 2, duration_i64),
    ("IMediaSeeking::GetCurrentPosition", 2, position_i64),
    ("IMediaSeeking::ConvertTimeFormat", 6, convert_time_format),
    ("IMediaSeeking::SetPositions", 5, set_positions),
    ("IMediaSeeking::GetPositions", 3, get_positions),
    ("IMediaSeeking::GetAvailable", 3, get_available),
    ("IMediaSeeking::SetRate", 3, ok),
    ("IMediaSeeking::GetRate", 2, one_f64),
    ("IMediaSeeking::GetPreroll", 2, zero_i64),
];

// ---- IBasicAudio ------------------------------------------------------------------------------

fn put_volume(c: &mut Cpu) -> u32 {
    let v = arg(c, 1) as i32;
    if !(-10000..=0).contains(&v) {
        return E_INVALIDARG;
    }
    with_graph(arg(c, 0), |g| {
        g.volume = v;
        if let Some(p) = &g.music {
            p.set_volume(v);
        }
    });
    S_OK
}

fn get_volume(c: &mut Cpu) -> u32 {
    let v = with_graph(arg(c, 0), |g| g.volume);
    put32(arg(c, 1), v as u32)
}

fn put_balance(c: &mut Cpu) -> u32 {
    let v = arg(c, 1) as i32;
    with_graph(arg(c, 0), |g| g.balance = v);
    S_OK
}

fn get_balance(c: &mut Cpu) -> u32 {
    let v = with_graph(arg(c, 0), |g| g.balance);
    put32(arg(c, 1), v as u32)
}

pub static BASIC_AUDIO: &[Import] = &[
    ("IBasicAudio::QueryInterface", 3, qi),
    ("IBasicAudio::AddRef", 1, add_ref),
    ("IBasicAudio::Release", 1, release),
    ("IBasicAudio::GetTypeInfoCount", 2, not_impl),
    ("IBasicAudio::GetTypeInfo", 4, not_impl),
    ("IBasicAudio::GetIDsOfNames", 6, not_impl),
    ("IBasicAudio::Invoke", 9, not_impl),
    ("IBasicAudio::put_Volume", 2, put_volume),
    ("IBasicAudio::get_Volume", 2, get_volume),
    ("IBasicAudio::put_Balance", 2, put_balance),
    ("IBasicAudio::get_Balance", 2, get_balance),
];

// ---- filters: the source (no pins) and the VMR9 -------------------------------------------------

fn get_class_id(c: &mut Cpu) -> u32 {
    let (this, p) = (arg(c, 0), arg(c, 1));
    if p == 0 {
        return E_POINTER;
    }
    let id = with(this, |o| if matches!(o.kind, Kind::Vmr9 { .. }) { CLSID_VMR9 } else { CLSID_ASYNC_READER });
    mem().slice(p, 16).copy_from_slice(&id);
    S_OK
}

/// GetState(timeout, out): stopped.
fn filter_state(c: &mut Cpu) -> u32 {
    put32(arg(c, 2), STOPPED)
}

fn null_out(c: &mut Cpu) -> u32 {
    put32(arg(c, 1), 0)
}

/// EnumPins(out): an enumerator of no pins.
fn enum_pins(c: &mut Cpu) -> u32 {
    let out = arg(c, 1);
    if out == 0 {
        return E_POINTER;
    }
    mem().wr32(out, new_object(Kind::EnumPins, &[(&[IID_IENUMPINS], ENUM_PINS)]));
    S_OK
}

/// FindPin(id, out): there are none.
fn find_pin(c: &mut Cpu) -> u32 {
    put32(arg(c, 2), 0);
    VFW_E_NOT_FOUND
}

pub static FILTER: &[Import] = &[
    ("IBaseFilter::QueryInterface", 3, qi),
    ("IBaseFilter::AddRef", 1, add_ref),
    ("IBaseFilter::Release", 1, release),
    ("IBaseFilter::GetClassID", 2, get_class_id),
    ("IBaseFilter::Stop", 1, ok),
    ("IBaseFilter::Pause", 1, ok),
    ("IBaseFilter::Run", 3, ok),
    ("IBaseFilter::GetState", 3, filter_state),
    ("IBaseFilter::SetSyncSource", 2, ok),
    ("IBaseFilter::GetSyncSource", 2, null_out),
    ("IBaseFilter::EnumPins", 2, enum_pins),
    ("IBaseFilter::FindPin", 3, find_pin),
    ("IBaseFilter::QueryFilterInfo", 2, nimp),
    ("IBaseFilter::JoinFilterGraph", 3, ok),
    ("IBaseFilter::QueryVendorInfo", 2, not_impl),
];

/// Next(count, pins, fetched): none.
fn pins_next(c: &mut Cpu) -> u32 {
    if arg(c, 3) != 0 {
        mem().wr32(arg(c, 3), 0);
    }
    if arg(c, 1) == 0 { S_OK } else { S_FALSE }
}

fn pins_skip(c: &mut Cpu) -> u32 {
    if arg(c, 1) == 0 { S_OK } else { S_FALSE }
}

pub static ENUM_PINS: &[Import] = &[
    ("IEnumPins::QueryInterface", 3, qi),
    ("IEnumPins::AddRef", 1, add_ref),
    ("IEnumPins::Release", 1, release),
    ("IEnumPins::Next", 4, pins_next),
    ("IEnumPins::Skip", 2, pins_skip),
    ("IEnumPins::Reset", 1, ok),
    ("IEnumPins::Clone", 2, enum_pins),
];

/// CoCreateInstance(CLSID_VideoMixingRenderer9).
pub fn create_vmr9(iid: u32, out: u32) -> u32 {
    create(
        Kind::Vmr9 { allocator: 0, mode: 1, streams: 1 },
        &[
            (&[IID_IPERSIST, IID_IMEDIAFILTER, IID_IBASEFILTER], FILTER),
            (&[IID_IVMRFILTERCONFIG9], VMR_CONFIG),
            (&[IID_IVMRSURFACEALLOCATORNOTIFY9], VMR_NOTIFY),
        ],
        iid,
        out,
    )
}

fn with_vmr<T>(this: u32, f: impl FnOnce(&mut u32, &mut u32, &mut u32) -> T) -> T {
    with(this, |o| match &mut o.kind {
        Kind::Vmr9 { allocator, mode, streams } => f(allocator, mode, streams),
        _ => panic!("not a VMR9"),
    })
}

fn set_streams(c: &mut Cpu) -> u32 {
    let n = arg(c, 1);
    with_vmr(arg(c, 0), |_, _, s| *s = n);
    S_OK
}

fn get_streams(c: &mut Cpu) -> u32 {
    let n = with_vmr(arg(c, 0), |_, _, s| *s);
    put32(arg(c, 1), n)
}

fn set_mode(c: &mut Cpu) -> u32 {
    let m = arg(c, 1);
    with_vmr(arg(c, 0), |_, mode, _| *mode = m);
    S_OK
}

fn get_mode(c: &mut Cpu) -> u32 {
    let m = with_vmr(arg(c, 0), |_, mode, _| *mode);
    put32(arg(c, 1), m)
}

pub static VMR_CONFIG: &[Import] = &[
    ("IVMRFilterConfig9::QueryInterface", 3, qi),
    ("IVMRFilterConfig9::AddRef", 1, add_ref),
    ("IVMRFilterConfig9::Release", 1, release),
    ("IVMRFilterConfig9::SetImageCompositor", 2, ok),
    ("IVMRFilterConfig9::SetNumberOfStreams", 2, set_streams),
    ("IVMRFilterConfig9::GetNumberOfStreams", 2, get_streams),
    ("IVMRFilterConfig9::SetRenderingPrefs", 2, ok),
    ("IVMRFilterConfig9::GetRenderingPrefs", 2, null_out),
    ("IVMRFilterConfig9::SetRenderingMode", 2, set_mode),
    ("IVMRFilterConfig9::GetRenderingMode", 2, get_mode),
];

/// AdviseSurfaceAllocator(id, allocator): the VMR9 holds a reference to the game's allocator.
fn advise_surface_allocator(c: &mut Cpu) -> u32 {
    let (this, a) = (arg(c, 0), arg(c, 2));
    if a != 0 {
        add_ref_any(c, a);
    }
    let old = with_vmr(this, |al, _, _| std::mem::replace(al, a));
    if old != 0 {
        release_any(c, old);
    }
    S_OK
}

pub static VMR_NOTIFY: &[Import] = &[
    ("IVMRSurfaceAllocatorNotify9::QueryInterface", 3, qi),
    ("IVMRSurfaceAllocatorNotify9::AddRef", 1, add_ref),
    ("IVMRSurfaceAllocatorNotify9::Release", 1, release),
    ("IVMRSurfaceAllocatorNotify9::AdviseSurfaceAllocator", 3, advise_surface_allocator),
    ("IVMRSurfaceAllocatorNotify9::SetD3DDevice", 3, ok),
    ("IVMRSurfaceAllocatorNotify9::ChangeD3DDevice", 3, ok),
    ("IVMRSurfaceAllocatorNotify9::AllocateSurfaceHelper", 4, nimp),
    ("IVMRSurfaceAllocatorNotify9::NotifyEvent", 4, ok),
];
