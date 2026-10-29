//! KERNEL32: process and C runtime start-up, memory, files, threads and synchronisation.

use super::{Import, ResId, alloc, alloc_aligned, arg, block_size, cstr, free, guest_call, host_path, log, mem, new_cstr, put_cstr, realloc, set_last_error};
use crate::rt::Cpu;
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

const INVALID_HANDLE: u32 = 0xffff_ffff;
pub const WAIT_TIMEOUT: u32 = 0x102;
const INFINITE: u32 = 0xffff_ffff;
const ERROR_FILE_NOT_FOUND: u32 = 2;
const ERROR_INVALID_HANDLE: u32 = 6;
const ERROR_ACCESS_DENIED: u32 = 5;

// ---- kernel objects -----------------------------------------------------------------------------

/// An open file: on the host, or on the CD image (read only: a file the CD has and the game
/// folder has not, `super::cd_file`).
pub enum File {
    Host(std::fs::File),
    Cd(crate::iso::IsoFile),
}

impl Read for File {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            File::Host(f) => f.read(buf),
            File::Cd(f) => f.read(buf),
        }
    }
}

impl Write for File {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            File::Host(f) => f.write(buf),
            File::Cd(_) => Err(std::io::ErrorKind::PermissionDenied.into()),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            File::Host(f) => f.flush(),
            File::Cd(_) => Ok(()),
        }
    }
}

impl Seek for File {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        match self {
            File::Host(f) => f.seek(to),
            File::Cd(f) => f.seek(to),
        }
    }
}

impl File {
    fn len(&self) -> u64 {
        match self {
            File::Host(f) => f.metadata().map_or(0, |m| m.len()),
            File::Cd(f) => f.size,
        }
    }

    fn set_len(&self, n: u64) -> std::io::Result<()> {
        match self {
            File::Host(f) => f.set_len(n),
            File::Cd(_) => Err(std::io::ErrorKind::PermissionDenied.into()),
        }
    }

    /// Created, accessed, modified (none for the CD's files).
    fn times(&self) -> [Option<std::time::SystemTime>; 3] {
        match self {
            File::Host(f) => f.metadata().map_or([None; 3], |m| [m.created().ok(), m.accessed().ok(), m.modified().ok()]),
            File::Cd(_) => [None; 3],
        }
    }
}

pub enum Obj {
    File(Arc<Mutex<File>>),
    /// standard input/output/error (-10, -11, -12)
    Std(u32),
    Event { manual: bool, set: bool },
    Mutex { owner: u32, count: u32 },
    Thread { id: u32, done: bool, code: u32, resume: Arc<(Mutex<bool>, Condvar)> },
    Heap,
}

pub struct Objects {
    map: BTreeMap<u32, Obj>,
    next: u32,
    /// critical sections by guest address: owner thread, recursion count
    cs: BTreeMap<u32, (u32, u32)>,
}

static OBJS: Mutex<Objects> = Mutex::new(Objects { map: BTreeMap::new(), next: 0x100, cs: BTreeMap::new() });
/// notified whenever an object becomes signalled
static SIGNAL: Condvar = Condvar::new();

pub fn objects() -> MutexGuard<'static, Objects> {
    OBJS.lock().unwrap_or_else(|e| e.into_inner())
}

impl Objects {
    pub fn add(&mut self, o: Obj) -> u32 {
        self.next += 4;
        self.map.insert(self.next, o);
        self.next
    }
}

fn wait_signal(g: MutexGuard<'static, Objects>, until: Option<Instant>) -> (MutexGuard<'static, Objects>, bool) {
    match until {
        None => (SIGNAL.wait(g).unwrap_or_else(|e| e.into_inner()), false),
        Some(t) => {
            let now = Instant::now();
            if now >= t {
                return (g, true);
            }
            let (g, r) = SIGNAL.wait_timeout(g, t - now).unwrap_or_else(|e| e.into_inner());
            (g, r.timed_out() && Instant::now() >= t)
        }
    }
}

static NEXT_TID: AtomicU32 = AtomicU32::new(1);
thread_local! {
    static TID: u32 = NEXT_TID.fetch_add(1, Ordering::Relaxed);
    static TLS: RefCell<Vec<u32>> = const { RefCell::new(Vec::new()) };
    static EXIT_CODE: Cell<u32> = const { Cell::new(0) };
}

pub fn thread_id() -> u32 {
    TID.with(|t| *t)
}

// ---- process ------------------------------------------------------------------------------------

/// Windows XP (5.1, build 2600), what the game was made for.
fn get_version(_: &mut Cpu) -> u32 {
    2600 << 16 | 1 << 8 | 5
}

/// GetVersionExA(OSVERSIONINFOA): Windows XP, as GetVersion.
fn get_version_ex(c: &mut Cpu) -> u32 {
    let p = arg(c, 0);
    let m = mem();
    let size = m.rd32(p);
    m.slice(p + 4, size.saturating_sub(4).min(152) as usize).fill(0);
    for (k, v) in [5u32, 1, 2600, 2].into_iter().enumerate() {
        m.wr32(p + 4 + 4 * k as u32, v); // major, minor, build, VER_PLATFORM_WIN32_NT
    }
    put_cstr(p + 20, "Service Pack 1", 128);
    1
}

fn get_command_line(_: &mut Cpu) -> u32 {
    static P: OnceLock<u32> = OnceLock::new();
    *P.get_or_init(|| new_cstr(&format!("\"{}\"", exe_path())))
}

fn get_startup_info(c: &mut Cpu) -> u32 {
    let p = arg(c, 0);
    mem().slice(p, 68).fill(0);
    mem().wr32(p, 68);
    0
}

fn get_module_handle(c: &mut Cpu) -> u32 {
    let p = arg(c, 0);
    if p == 0 {
        return super::IMAGE_BASE;
    }
    super::system_module(&cstr(p)).unwrap_or(0)
}

/// The installed game (see `host_path`): `INSTALL_DIR`\Gunmetal.exe.
pub fn exe_path() -> String {
    format!("{}\\{}", super::INSTALL_DIR, crate::game::EXE)
}

fn get_module_file_name(c: &mut Cpu) -> u32 {
    let (buf, n) = (arg(c, 1), arg(c, 2));
    put_cstr(buf, &exe_path(), n)
}

fn get_environment_strings(_: &mut Cpu) -> u32 {
    alloc(4) // empty block: two NULs (also as UTF-16)
}

fn exit_process(c: &mut Cpu) -> u32 {
    let code = arg(c, 0);
    super::advapi32::save();
    log!("ExitProcess({code})");
    std::process::exit(code as i32)
}

fn exit_thread(c: &mut Cpu) -> u32 {
    EXIT_CODE.set(arg(c, 0));
    c.stop = 1;
    0
}

fn get_system_info(c: &mut Cpu) -> u32 {
    let p = arg(c, 0);
    let m = mem();
    m.slice(p, 36).fill(0);
    m.wr32(p + 4, 0x1000); // page size
    m.wr32(p + 8, 0x10000);
    m.wr32(p + 12, 0x7ffe_ffff);
    m.wr32(p + 16, 1);
    m.wr32(p + 20, 1); // processors
    m.wr32(p + 24, 586); // PROCESSOR_INTEL_PENTIUM, as Windows says for every later one
    m.wr32(p + 28, 0x10000);
    m.wr16(p + 32, 6); // level: family 6, as cpuid says (`rt::rt_cpuid`)
    m.wr16(p + 34, 0x0100); // model 1, stepping 0
    0
}

fn get_computer_name(c: &mut Cpu) -> u32 {
    let (buf, len) = (arg(c, 0), arg(c, 1));
    let n = mem().rd32(len);
    let k = put_cstr(buf, "GUNMETAL", n);
    mem().wr32(len, k);
    1
}

fn output_debug_string(c: &mut Cpu) -> u32 {
    log!("debug: {}", cstr(arg(c, 0)).trim_end());
    0
}

fn raise_exception(c: &mut Cpu) -> u32 {
    log!("RaiseException {:#x}", arg(c, 0));
    std::process::exit(102)
}

// ---- drives (the CD check) ----------------------------------------------------------------------

fn get_logical_drives(_: &mut Cpu) -> u32 {
    1 << 2 | 1 << (super::CD_DRIVE - b'A') // C: and the CD
}

fn get_drive_type(c: &mut Cpu) -> u32 {
    let p = arg(c, 0);
    match if p == 0 { b'C' } else { mem().rd8(p).to_ascii_uppercase() } {
        b'C' => 3,                    // fixed
        d if d == super::CD_DRIVE => 5, // CD-ROM
        _ => 1,                       // no root directory
    }
}

/// The CD's volume label: the one of the CD image (`super::cd_image`), else `CD_LABEL`.
pub fn cd_label() -> &'static str {
    static L: OnceLock<String> = OnceLock::new();
    L.get_or_init(|| match super::cd_image().and_then(crate::iso::Iso::open) {
        Some(i) => {
            log!("CD: {} (volume {})", super::cd_image().unwrap().display(), i.label);
            i.label
        }
        None => CD_LABEL.to_string(),
    })
}

/// The volume label without a CD image. The game's CD check (0x459240) only needs
/// GetVolumeInformationA to succeed for the drive, it compares no label; this is the user's
/// CD's ("NEW", as its image says).
pub const CD_LABEL: &str = "NEW";

fn get_volume_information(c: &mut Cpu) -> u32 {
    let root = arg(c, 0);
    let cd = root != 0 && mem().rd8(root).eq_ignore_ascii_case(&super::CD_DRIVE);
    let m = mem();
    put_cstr(arg(c, 1), if cd { cd_label() } else { "SYSTEM" }, arg(c, 2));
    for (k, v) in [(3, 0x1998_0001u32), (4, 255), (5, 0)] {
        if arg(c, k) != 0 {
            m.wr32(arg(c, k), v);
        }
    }
    put_cstr(arg(c, 6), if cd { "CDFS" } else { "FAT32" }, arg(c, 7));
    1
}

// ---- strings and code pages ---------------------------------------------------------------------

fn get_acp(_: &mut Cpu) -> u32 {
    1252
}
fn get_oemcp(_: &mut Cpu) -> u32 {
    437
}

fn get_cp_info(c: &mut Cpu) -> u32 {
    let p = arg(c, 1);
    let m = mem();
    m.slice(p, 20).fill(0);
    m.wr32(p, 1); // single-byte
    m.wr8(p + 4, b'?');
    1
}

/// Length of a string argument given as (pointer, count), count -1 meaning NUL-terminated
/// including the NUL.
fn str_len(p: u32, n: u32, wide: bool) -> u32 {
    if n != 0xffff_ffff {
        return n;
    }
    let m = mem();
    let mut k = 0;
    while if wide { m.rd16(p + 2 * k) != 0 } else { m.rd8(p + k) != 0 } {
        k += 1;
    }
    k + 1
}

fn multi_byte_to_wide_char(c: &mut Cpu) -> u32 {
    let (src, n, dst, dn) = (arg(c, 2), arg(c, 3), arg(c, 4), arg(c, 5));
    let n = str_len(src, n, false);
    if dn == 0 {
        return n;
    }
    let m = mem();
    let k = n.min(dn);
    for i in 0..k {
        m.wr16(dst + 2 * i, m.rd8(src + i) as u16);
    }
    k
}

fn wide_char_to_multi_byte(c: &mut Cpu) -> u32 {
    let (src, n, dst, dn) = (arg(c, 2), arg(c, 3), arg(c, 4), arg(c, 5));
    let n = str_len(src, n, true);
    if dn == 0 {
        return n;
    }
    let m = mem();
    let k = n.min(dn);
    for i in 0..k {
        let w = m.rd16(src + 2 * i);
        m.wr8(dst + i, if w < 256 { w as u8 } else { b'?' });
    }
    k
}

fn ctype(ch: u16) -> u16 {
    let Ok(b) = u8::try_from(ch) else { return 0x100 };
    let c = b as char;
    let mut t = 0;
    if c.is_ascii_uppercase() || (0xc0..=0xde).contains(&b) && b != 0xd7 {
        t |= 0x101;
    }
    if c.is_ascii_lowercase() || (0xdf..=0xff).contains(&b) && b != 0xf7 {
        t |= 0x102;
    }
    if c.is_ascii_digit() {
        t |= 4;
    }
    if matches!(b, 9..=13 | 32) {
        t |= 8;
    }
    if c.is_ascii_punctuation() {
        t |= 0x10;
    }
    if b < 32 || b == 127 {
        t |= 0x20;
    }
    if b == 9 || b == 32 {
        t |= 0x40;
    }
    if c.is_ascii_hexdigit() {
        t |= 0x80;
    }
    t
}

fn get_string_type_a(c: &mut Cpu) -> u32 {
    let (src, n, out) = (arg(c, 2), arg(c, 3), arg(c, 4));
    let n = str_len(src, n, false);
    for i in 0..n {
        mem().wr16(out + 2 * i, ctype(mem().rd8(src + i) as u16));
    }
    1
}

fn get_string_type_w(c: &mut Cpu) -> u32 {
    let (src, n, out) = (arg(c, 1), arg(c, 2), arg(c, 3));
    let n = str_len(src, n, true);
    for i in 0..n {
        mem().wr16(out + 2 * i, ctype(mem().rd16(src + 2 * i)));
    }
    1
}

fn lc_map(c: &mut Cpu, wide: bool) -> u32 {
    let (flags, src, n, dst, dn) = (arg(c, 1), arg(c, 2), arg(c, 3), arg(c, 4), arg(c, 5));
    let n = str_len(src, n, wide);
    if dn == 0 {
        return n;
    }
    let m = mem();
    let k = n.min(dn);
    for i in 0..k {
        let v = if wide { m.rd16(src + 2 * i) as u8 } else { m.rd8(src + i) };
        let v = match flags & 0x300 {
            0x100 => v.to_ascii_lowercase(),
            0x200 => v.to_ascii_uppercase(),
            _ => v,
        };
        if wide { m.wr16(dst + 2 * i, v as u16) } else { m.wr8(dst + i, v) }
    }
    k
}
fn lc_map_string_a(c: &mut Cpu) -> u32 {
    lc_map(c, false)
}
fn lc_map_string_w(c: &mut Cpu) -> u32 {
    lc_map(c, true)
}

fn compare(c: &mut Cpu, wide: bool) -> u32 {
    let (flags, a, an, b, bn) = (arg(c, 1), arg(c, 2), arg(c, 3), arg(c, 4), arg(c, 5));
    let get = |p: u32, n: u32| -> Vec<u8> {
        let n = str_len(p, n, wide);
        let mut v: Vec<u8> = (0..n).map(|i| if wide { mem().rd16(p + 2 * i) as u8 } else { mem().rd8(p + i) }).collect();
        while v.last() == Some(&0) {
            v.pop();
        }
        if flags & 1 != 0 {
            v.make_ascii_lowercase();
        }
        v
    };
    match get(a, an).cmp(&get(b, bn)) {
        std::cmp::Ordering::Less => 1,
        std::cmp::Ordering::Equal => 2,
        std::cmp::Ordering::Greater => 3,
    }
}
fn compare_string_a(c: &mut Cpu) -> u32 {
    compare(c, false)
}
fn compare_string_w(c: &mut Cpu) -> u32 {
    compare(c, true)
}

fn locale_info(kind: u32) -> &'static str {
    match kind & 0xffff {
        0x1004 => "1252", // LOCALE_IDEFAULTANSICODEPAGE
        0xb => "437",     // LOCALE_IDEFAULTCODEPAGE
        0x1 => "0409",    // LOCALE_ILANGUAGE
        0x3 => "ENU",
        0x7 => "USA",
        0xe => ".",
        0xf => ",",
        _ => "",
    }
}

fn get_locale_info_a(c: &mut Cpu) -> u32 {
    let (kind, buf, n) = (arg(c, 1), arg(c, 2), arg(c, 3));
    let s = locale_info(kind);
    if n == 0 { s.len() as u32 + 1 } else { put_cstr(buf, s, n) + 1 }
}

fn get_locale_info_w(c: &mut Cpu) -> u32 {
    let (kind, buf, n) = (arg(c, 1), arg(c, 2), arg(c, 3));
    let s = locale_info(kind);
    if n == 0 {
        return s.len() as u32 + 1;
    }
    let k = (s.len() as u32).min(n - 1);
    for (i, b) in s.bytes().take(k as usize).enumerate() {
        mem().wr16(buf + 2 * i as u32, b as u16);
    }
    mem().wr16(buf + 2 * k, 0);
    k + 1
}

fn lstrlen(c: &mut Cpu) -> u32 {
    let p = arg(c, 0);
    if p == 0 { 0 } else { str_len(p, 0xffff_ffff, false) - 1 }
}

fn lstrcpy(c: &mut Cpu) -> u32 {
    let (d, s) = (arg(c, 0), arg(c, 1));
    let n = str_len(s, 0xffff_ffff, false);
    let b = mem().slice(s, n as usize).to_vec();
    mem().slice(d, n as usize).copy_from_slice(&b);
    d
}

// ---- memory -------------------------------------------------------------------------------------

fn heap_create(_: &mut Cpu) -> u32 {
    objects().add(Obj::Heap)
}

fn heap_alloc(c: &mut Cpu) -> u32 {
    alloc(arg(c, 2))
}

fn heap_free(c: &mut Cpu) -> u32 {
    free(arg(c, 2)) as u32
}

fn heap_realloc(c: &mut Cpu) -> u32 {
    // HEAP_REALLOC_IN_PLACE_ONLY: only when it fits already
    let (flags, p, n) = (arg(c, 1), arg(c, 2), arg(c, 3));
    if flags & 0x10 != 0 {
        return if block_size(p) >= n { p } else { 0 };
    }
    realloc(p, n)
}

fn heap_size(c: &mut Cpu) -> u32 {
    let p = arg(c, 2);
    match block_size(p) {
        0 => 0xffff_ffff,
        n => n,
    }
}

fn virtual_alloc(c: &mut Cpu) -> u32 {
    let (p, n) = (arg(c, 0), arg(c, 1));
    if p != 0 {
        // committing pages of an earlier reservation: ours are committed already
        return p;
    }
    alloc_aligned(n, 0x1000)
}

fn virtual_free(c: &mut Cpu) -> u32 {
    let (p, kind) = (arg(c, 0), arg(c, 2));
    if kind & 0x8000 != 0 {
        free(p);
    }
    1
}

fn virtual_protect(c: &mut Cpu) -> u32 {
    let old = arg(c, 3);
    if old != 0 {
        mem().wr32(old, 4); // PAGE_READWRITE
    }
    1
}

fn global_alloc(c: &mut Cpu) -> u32 {
    alloc(arg(c, 1))
}
fn global_free(c: &mut Cpu) -> u32 {
    free(arg(c, 0));
    0
}
/// GlobalLock, GlobalUnlock, GlobalHandle: our handles are the memory itself
fn global_same(c: &mut Cpu) -> u32 {
    arg(c, 0)
}
fn global_unlock(_: &mut Cpu) -> u32 {
    0
}

// ---- thread-local storage, critical sections, interlocked ---------------------------------------

static TLS_NEXT: AtomicU32 = AtomicU32::new(0);

fn tls_alloc(_: &mut Cpu) -> u32 {
    TLS_NEXT.fetch_add(1, Ordering::Relaxed)
}

fn tls_get_value(c: &mut Cpu) -> u32 {
    let k = arg(c, 0) as usize;
    set_last_error(0);
    TLS.with(|t| t.borrow().get(k).copied().unwrap_or(0))
}

fn tls_set_value(c: &mut Cpu) -> u32 {
    let (k, v) = (arg(c, 0) as usize, arg(c, 1));
    TLS.with(|t| {
        let mut t = t.borrow_mut();
        if t.len() <= k {
            t.resize(k + 1, 0);
        }
        t[k] = v;
    });
    1
}

fn init_cs(c: &mut Cpu) -> u32 {
    let p = arg(c, 0);
    mem().slice(p, 24).fill(0);
    objects().cs.insert(p, (0, 0));
    0
}

fn delete_cs(c: &mut Cpu) -> u32 {
    objects().cs.remove(&arg(c, 0));
    0
}

fn enter_cs(c: &mut Cpu) -> u32 {
    let p = arg(c, 0);
    let me = thread_id();
    let mut g = objects();
    loop {
        let e = g.cs.entry(p).or_insert((0, 0));
        if e.0 == 0 || e.0 == me {
            e.0 = me;
            e.1 += 1;
            return 0;
        }
        g = wait_signal(g, None).0;
    }
}

fn leave_cs(c: &mut Cpu) -> u32 {
    let p = arg(c, 0);
    let mut g = objects();
    if let Some(e) = g.cs.get_mut(&p) {
        e.1 = e.1.saturating_sub(1);
        if e.1 == 0 {
            e.0 = 0;
            SIGNAL.notify_all();
        }
    }
    0
}

fn interlocked(c: &mut Cpu, d: u32) -> u32 {
    let p = arg(c, 0);
    let _g = objects();
    let v = mem().rd32(p).wrapping_add(d);
    mem().wr32(p, v);
    v
}
fn interlocked_increment(c: &mut Cpu) -> u32 {
    interlocked(c, 1)
}
fn interlocked_decrement(c: &mut Cpu) -> u32 {
    interlocked(c, u32::MAX)
}

// ---- events, mutexes, waiting -------------------------------------------------------------------

fn create_event(c: &mut Cpu) -> u32 {
    objects().add(Obj::Event { manual: arg(c, 1) != 0, set: arg(c, 2) != 0 })
}

/// Set or reset event `h` (also from host threads); 0 if it is no event.
pub fn set_event_state(h: u32, on: bool) -> u32 {
    let mut g = objects();
    match g.map.get_mut(&h) {
        Some(Obj::Event { set, .. }) => {
            *set = on;
            SIGNAL.notify_all();
            1
        }
        _ => 0,
    }
}
fn set_event(c: &mut Cpu) -> u32 {
    set_event_state(arg(c, 0), true)
}
fn reset_event(c: &mut Cpu) -> u32 {
    set_event_state(arg(c, 0), false)
}
fn pulse_event(c: &mut Cpu) -> u32 {
    // wakes the waiters there are now: approximated by a short signalled moment
    let h = arg(c, 0);
    set_event_state(h, true);
    std::thread::yield_now();
    set_event_state(h, false)
}

fn create_mutex(c: &mut Cpu) -> u32 {
    let owner = if arg(c, 1) != 0 { thread_id() } else { 0 };
    objects().add(Obj::Mutex { owner, count: (owner != 0) as u32 })
}

/// Whether `h` is signalled for this thread; with `take`, also what a successful wait does to it.
fn signalled(g: &mut Objects, h: u32, take: bool) -> bool {
    let me = thread_id();
    match g.map.get_mut(&h) {
        Some(Obj::Event { manual, set }) => {
            let s = *set;
            if s && take && !*manual {
                *set = false;
            }
            s
        }
        Some(Obj::Mutex { owner, count }) => {
            let s = *owner == 0 || *owner == me;
            if s && take {
                *owner = me;
                *count += 1;
            }
            s
        }
        Some(Obj::Thread { done, .. }) => *done,
        _ => true,
    }
}

/// Wait for handles (any or all) up to `ms`: the index of the one signalled, or WAIT_TIMEOUT.
pub fn wait(handles: &[u32], all: bool, ms: u32) -> u32 {
    let until = (ms != INFINITE).then(|| Instant::now() + Duration::from_millis(ms as u64));
    let mut g = objects();
    loop {
        if all {
            if handles.iter().all(|&h| signalled(&mut g, h, false)) {
                for &h in handles {
                    signalled(&mut g, h, true);
                }
                return 0;
            }
        } else if let Some(k) = handles.iter().position(|&h| signalled(&mut g, h, false)) {
            signalled(&mut g, handles[k], true);
            return k as u32;
        }
        let (ng, timed_out) = wait_signal(g, until);
        g = ng;
        if timed_out {
            return WAIT_TIMEOUT;
        }
    }
}

fn wait_for_single_object(c: &mut Cpu) -> u32 {
    wait(&[arg(c, 0)], false, arg(c, 1))
}

fn wait_for_multiple_objects(c: &mut Cpu) -> u32 {
    let (n, p, all, ms) = (arg(c, 0), arg(c, 1), arg(c, 2), arg(c, 3));
    let hs: Vec<u32> = (0..n).map(|k| mem().rd32(p + 4 * k)).collect();
    wait(&hs, all != 0, ms)
}

fn close_handle(c: &mut Cpu) -> u32 {
    let h = arg(c, 0);
    let mut g = objects();
    // a thread handle stays until the thread ends (it only marks itself done)
    if matches!(g.map.get(&h), Some(Obj::Thread { done: false, .. })) {
        return 1;
    }
    if g.map.remove(&h).is_some() {
        1
    } else {
        set_last_error(ERROR_INVALID_HANDLE);
        0
    }
}

fn duplicate_handle(c: &mut Cpu) -> u32 {
    let (src, out) = (arg(c, 1), arg(c, 3));
    let h = if src == 0xffff_fffe { thread_handle_of(thread_id()) } else { src };
    if out != 0 {
        mem().wr32(out, h);
    }
    1
}

// ---- threads ------------------------------------------------------------------------------------

fn thread_handle_of(id: u32) -> u32 {
    let g = objects();
    g.map.iter().find(|(_, o)| matches!(o, Obj::Thread { id: i, .. } if *i == id)).map(|(&h, _)| h).unwrap_or(0xffff_fffe)
}

fn create_thread(c: &mut Cpu) -> u32 {
    let (start, param, flags, id_out) = (arg(c, 2), arg(c, 3), arg(c, 4), arg(c, 5));
    let resume = Arc::new((Mutex::new(flags & 4 == 0), Condvar::new()));
    let r2 = resume.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(move || {
            let id = thread_id();
            tx.send(id).unwrap();
            {
                let (m, cv) = &*r2;
                let mut go = m.lock().unwrap();
                while !*go {
                    go = cv.wait(go).unwrap();
                }
            }
            let mut tc = super::new_thread_cpu();
            let r = guest_call(&mut tc, start, &[param]);
            let code = if tc.stop != 0 { EXIT_CODE.get() } else { r };
            let mut g = objects();
            for o in g.map.values_mut() {
                if let Obj::Thread { id: i, done, code: c, .. } = o {
                    if *i == id {
                        *done = true;
                        *c = code;
                    }
                }
            }
            SIGNAL.notify_all();
        })
        .expect("cannot start a thread");
    let id = rx.recv().unwrap();
    if id_out != 0 {
        mem().wr32(id_out, id);
    }
    objects().add(Obj::Thread { id, done: false, code: 0, resume })
}

fn resume_thread(c: &mut Cpu) -> u32 {
    let g = objects();
    if let Some(Obj::Thread { resume, .. }) = g.map.get(&arg(c, 0)) {
        let (m, cv) = &**resume;
        let was = !*m.lock().unwrap();
        *m.lock().unwrap() = true;
        cv.notify_all();
        return was as u32;
    }
    0xffff_ffff
}

fn terminate_thread(c: &mut Cpu) -> u32 {
    log!("warn: TerminateThread({:#x}) ignored", arg(c, 0));
    1
}

fn get_current_thread_id(_: &mut Cpu) -> u32 {
    thread_id()
}

// ---- time ---------------------------------------------------------------------------------------

pub fn start_time() -> Instant {
    static T: OnceLock<Instant> = OnceLock::new();
    *T.get_or_init(Instant::now)
}

pub fn ms_since_start() -> u32 {
    start_time().elapsed().as_millis() as u32
}

fn get_tick_count(_: &mut Cpu) -> u32 {
    ms_since_start()
}

fn query_performance_frequency(c: &mut Cpu) -> u32 {
    let p = arg(c, 0);
    mem().wr32(p, 1_000_000);
    mem().wr32(p + 4, 0);
    1
}

fn query_performance_counter(c: &mut Cpu) -> u32 {
    let p = arg(c, 0);
    let t = start_time().elapsed().as_micros() as u64;
    mem().wr32(p, t as u32);
    mem().wr32(p + 4, (t >> 32) as u32);
    1
}

fn sleep(c: &mut Cpu) -> u32 {
    let ms = arg(c, 0);
    if ms == 0 { std::thread::yield_now() } else { std::thread::sleep(Duration::from_millis(ms as u64)) }
    0
}

// ---- files --------------------------------------------------------------------------------------

fn create_file(c: &mut Cpu) -> u32 {
    let (name, access, disp) = (cstr(arg(c, 0)), arg(c, 1), arg(c, 4));
    let path = host_path(&name);
    let write = access & 0x4000_0000 != 0;
    if write && super::on_cd(&name) {
        set_last_error(ERROR_ACCESS_DENIED);
        return INVALID_HANDLE;
    }
    let mut o = std::fs::OpenOptions::new();
    o.read(access & 0x8000_0000 != 0 || !write).write(write);
    match disp {
        1 => o.create_new(true),
        2 => o.create(true).truncate(true),
        4 => o.create(true),
        5 => o.truncate(true),
        _ => &mut o,
    };
    let opened = o.open(&path).map(File::Host).or_else(|e| match super::cd_file(&name) {
        Some(f) if e.kind() == std::io::ErrorKind::NotFound && !write => Ok(File::Cd(f)),
        _ => Err(e),
    });
    match opened {
        Ok(f) => {
            if super::trace_api() {
                let from = if matches!(f, File::Cd(_)) { " from the CD image" } else { "" };
                log!("CreateFileA {name} (access {access:#x}, disposition {disp}){from}");
            }
            objects().add(Obj::File(Arc::new(Mutex::new(f))))
        }
        Err(e) => {
            if super::trace_api() {
                log!("CreateFileA {name}: {e}");
            }
            set_last_error(if e.kind() == std::io::ErrorKind::AlreadyExists { 80 } else { ERROR_FILE_NOT_FOUND });
            INVALID_HANDLE
        }
    }
}

fn file_of(h: u32) -> Option<Arc<Mutex<File>>> {
    match objects().map.get(&h) {
        Some(Obj::File(f)) => Some(f.clone()),
        _ => None,
    }
}

fn read_file(c: &mut Cpu) -> u32 {
    let (h, buf, n, done) = (arg(c, 0), arg(c, 1), arg(c, 2), arg(c, 3));
    let Some(f) = file_of(h) else {
        set_last_error(ERROR_INVALID_HANDLE);
        return 0;
    };
    let dst = mem().slice(buf, n as usize);
    let mut f = f.lock().unwrap();
    let mut got = 0;
    while got < dst.len() {
        match f.read(&mut dst[got..]) {
            Ok(0) => break,
            Ok(k) => got += k,
            Err(_) => return 0,
        }
    }
    if done != 0 {
        mem().wr32(done, got as u32);
    }
    1
}

fn write_file(c: &mut Cpu) -> u32 {
    let (h, buf, n, done) = (arg(c, 0), arg(c, 1), arg(c, 2), arg(c, 3));
    let data = mem().slice(buf, n as usize).to_vec();
    let ok = match objects().map.get(&h) {
        Some(Obj::Std(_)) => {
            eprint!("{}", String::from_utf8_lossy(&data));
            true
        }
        Some(Obj::File(f)) => f.lock().unwrap().write_all(&data).is_ok(),
        _ => false,
    };
    if done != 0 {
        mem().wr32(done, if ok { n } else { 0 });
    }
    ok as u32
}

fn set_file_pointer(c: &mut Cpu) -> u32 {
    let (h, lo, hi_p, how) = (arg(c, 0), arg(c, 1), arg(c, 2), arg(c, 3));
    let Some(f) = file_of(h) else {
        set_last_error(ERROR_INVALID_HANDLE);
        return 0xffff_ffff;
    };
    let d = if hi_p != 0 { ((mem().rd32(hi_p) as u64) << 32 | lo as u64) as i64 } else { lo as i32 as i64 };
    let pos = match how {
        0 => SeekFrom::Start(d as u64),
        1 => SeekFrom::Current(d),
        _ => SeekFrom::End(d),
    };
    match f.lock().unwrap().seek(pos) {
        Ok(p) => {
            if hi_p != 0 {
                mem().wr32(hi_p, (p >> 32) as u32);
            }
            p as u32
        }
        Err(_) => {
            set_last_error(131);
            0xffff_ffff
        }
    }
}

fn set_end_of_file(c: &mut Cpu) -> u32 {
    let Some(f) = file_of(arg(c, 0)) else { return 0 };
    let mut f = f.lock().unwrap();
    let Ok(p) = f.stream_position() else { return 0 };
    f.set_len(p).is_ok() as u32
}

fn flush_file_buffers(c: &mut Cpu) -> u32 {
    file_of(arg(c, 0)).map_or(0, |f| f.lock().unwrap().flush().is_ok() as u32)
}

fn get_std_handle(c: &mut Cpu) -> u32 {
    let n = arg(c, 0);
    let mut g = objects();
    if let Some((&h, _)) = g.map.iter().find(|(_, o)| matches!(o, Obj::Std(k) if *k == n)) {
        return h;
    }
    g.add(Obj::Std(n))
}

fn get_file_type(c: &mut Cpu) -> u32 {
    match objects().map.get(&arg(c, 0)) {
        Some(Obj::File(_)) => 1,
        Some(Obj::Std(_)) => 2,
        _ => 0,
    }
}

// ---- simple ones --------------------------------------------------------------------------------

fn ret0(_: &mut Cpu) -> u32 {
    0
}
fn ret1(_: &mut Cpu) -> u32 {
    1
}
fn get_last_error(_: &mut Cpu) -> u32 {
    super::LAST_ERROR.get()
}
fn set_last_error_imp(c: &mut Cpu) -> u32 {
    set_last_error(arg(c, 0));
    0
}
fn get_current_process(_: &mut Cpu) -> u32 {
    0xffff_ffff
}
fn get_current_thread(_: &mut Cpu) -> u32 {
    0xffff_fffe
}
fn first_arg(c: &mut Cpu) -> u32 {
    arg(c, 0)
}
/// The DLLs the game loads itself are system DLLs we implement (d3d9.dll: `d3d9.rs`).
fn load_library(c: &mut Cpu) -> u32 {
    let name = cstr(arg(c, 0));
    if let Some(h) = super::system_module(&name) {
        return h;
    }
    log!("LoadLibraryA({name}): not available");
    set_last_error(126);
    0
}
fn get_proc_address(c: &mut Cpu) -> u32 {
    let (module, name) = (arg(c, 0), arg(c, 1));
    let name = if name > 0xffff { cstr(name) } else { format!("#{name}") };
    let a = super::system_proc(module, &name);
    if a != 0 {
        return a;
    }
    log!("GetProcAddress({module:#x}, {name}): not available");
    0
}

// ---- resources ----------------------------------------------------------------------------------

/// FindResourceA(module, name, type): a guest record of the data's address and size.
fn find_resource(c: &mut Cpu) -> u32 {
    let base = if arg(c, 0) == 0 { super::IMAGE_BASE } else { arg(c, 0) };
    let (name, ty) = (ResId::from_guest(arg(c, 1)), ResId::from_guest(arg(c, 2)));
    match super::resource_in(base, ty, name) {
        Some((a, n)) => {
            let r = alloc(8);
            mem().wr32(r, a);
            mem().wr32(r + 4, n);
            r
        }
        None => {
            log!("FindResourceA({name:?}, {ty:?}): not found");
            set_last_error(1814); // ERROR_RESOURCE_NAME_NOT_FOUND
            0
        }
    }
}
fn load_resource(c: &mut Cpu) -> u32 {
    let r = arg(c, 1);
    if r == 0 { 0 } else { mem().rd32(r) }
}

// ---- old 16-bit style files (_lopen) --------------------------------------------------------------

fn lopen(c: &mut Cpu) -> u32 {
    let (name, mode) = (cstr(arg(c, 0)), arg(c, 1) & 3);
    if mode != 0 && super::on_cd(&name) {
        set_last_error(ERROR_ACCESS_DENIED);
        return INVALID_HANDLE;
    }
    let mut o = std::fs::OpenOptions::new();
    o.read(mode != 1).write(mode != 0);
    match o.open(host_path(&name)) {
        Ok(f) => objects().add(Obj::File(Arc::new(Mutex::new(File::Host(f))))),
        Err(_) => {
            if super::trace_api() {
                log!("_lopen {name}: not found");
            }
            set_last_error(ERROR_FILE_NOT_FOUND);
            INVALID_HANDLE
        }
    }
}
fn lread(c: &mut Cpu) -> u32 {
    let (h, buf, n) = (arg(c, 0), arg(c, 1), arg(c, 2));
    let Some(f) = file_of(h) else { return INVALID_HANDLE };
    let dst = mem().slice(buf, n as usize);
    let mut f = f.lock().unwrap();
    let mut got = 0;
    while got < dst.len() {
        match f.read(&mut dst[got..]) {
            Ok(0) => break,
            Ok(k) => got += k,
            Err(_) => return INVALID_HANDLE,
        }
    }
    got as u32
}
fn lclose(c: &mut Cpu) -> u32 {
    objects().map.remove(&arg(c, 0));
    0
}

fn get_file_size(c: &mut Cpu) -> u32 {
    let (h, hi) = (arg(c, 0), arg(c, 1));
    let Some(f) = file_of(h) else {
        set_last_error(ERROR_INVALID_HANDLE);
        return INVALID_HANDLE;
    };
    let n = f.lock().unwrap().len();
    if hi != 0 {
        mem().wr32(hi, (n >> 32) as u32);
    }
    n as u32
}
fn delete_file(c: &mut Cpu) -> u32 {
    let name = cstr(arg(c, 0));
    if !super::on_cd(&name) && std::fs::remove_file(host_path(&name)).is_ok() {
        1
    } else {
        set_last_error(ERROR_FILE_NOT_FOUND);
        0
    }
}
fn set_current_directory(c: &mut Cpu) -> u32 {
    let d = cstr(arg(c, 0));
    if !host_path(&d).is_dir() {
        set_last_error(3); // ERROR_PATH_NOT_FOUND
        return 0;
    }
    super::set_current_dir(&d);
    if super::trace_api() {
        log!("SetCurrentDirectoryA {d} -> {}", super::current_dir());
    }
    1
}

fn get_current_directory(c: &mut Cpu) -> u32 {
    let (n, buf) = (arg(c, 0), arg(c, 1));
    let d = super::current_dir();
    // too small: the size needed, with the NUL
    if (d.len() as u32) < n { put_cstr(buf, &d, n) } else { d.len() as u32 + 1 }
}

/// GetFullPathNameA(name, n, buf, file part).
fn get_full_path_name(c: &mut Cpu) -> u32 {
    let (name, n, buf, part) = (cstr(arg(c, 0)), arg(c, 1), arg(c, 2), arg(c, 3));
    let full = super::full_path(&name);
    if full.len() as u32 >= n {
        return full.len() as u32 + 1;
    }
    let k = put_cstr(buf, &full, n);
    if part != 0 {
        let file = full.rfind('\\').map_or(0, |i| i + 1) as u32;
        mem().wr32(part, if file < k { buf + file } else { 0 });
    }
    k
}

fn get_file_attributes(c: &mut Cpu) -> u32 {
    let name = cstr(arg(c, 0));
    match std::fs::metadata(host_path(&name)) {
        Ok(m) => {
            let ro = m.permissions().readonly() || super::on_cd(&name);
            (if m.is_dir() { 0x10 } else { 0x80 }) | ro as u32 // DIRECTORY or NORMAL, READONLY
        }
        Err(_) => {
            if super::trace_api() {
                log!("GetFileAttributesA {name}: not found");
            }
            set_last_error(ERROR_FILE_NOT_FOUND);
            INVALID_HANDLE
        }
    }
}

fn create_directory(c: &mut Cpu) -> u32 {
    let name = cstr(arg(c, 0));
    let p = host_path(&name);
    if p.is_dir() {
        set_last_error(183); // ERROR_ALREADY_EXISTS
        return 0;
    }
    if super::on_cd(&name) || std::fs::create_dir(&p).is_err() {
        set_last_error(ERROR_ACCESS_DENIED);
        return 0;
    }
    1
}

fn move_file(c: &mut Cpu) -> u32 {
    let (from, to) = (cstr(arg(c, 0)), cstr(arg(c, 1)));
    if super::on_cd(&from) || super::on_cd(&to) || std::fs::rename(host_path(&from), host_path(&to)).is_err() {
        set_last_error(ERROR_ACCESS_DENIED);
        return 0;
    }
    1
}

// ---- finding files ------------------------------------------------------------------------------

/// FindFirstFileA(pattern, WIN32_FIND_DATAA): the first file matching `*`/`?` in the last
/// part. The game imports no FindNextFileA: only whether one exists (and its data) counts.
fn find_first_file(c: &mut Cpu) -> u32 {
    let (pattern, out) = (cstr(arg(c, 0)), arg(c, 1));
    let (dir, pat) = pattern.rsplit_once(['\\', '/']).unwrap_or(("", &pattern));
    let host = if dir.is_empty() { host_path(".") } else { host_path(dir) };
    let found = std::fs::read_dir(&host).ok().and_then(|d| d.flatten().find(|e| wildcard(pat, &e.file_name().to_string_lossy())));
    if super::trace_api() {
        log!("FindFirstFileA {pattern}: {:?}", found.as_ref().map(|e| e.file_name()));
    }
    let Some(e) = found else {
        set_last_error(ERROR_FILE_NOT_FOUND);
        return INVALID_HANDLE;
    };
    let m = mem();
    m.slice(out, 320).fill(0);
    let meta = e.metadata().ok();
    let dir = meta.as_ref().is_some_and(|m| m.is_dir());
    m.wr32(out, if dir { 0x10 } else { 0x80 });
    let t = meta.as_ref().and_then(|m| m.modified().ok()).map_or(0, file_time);
    for o in [4, 12, 20] {
        m.wr32(out + o, t as u32);
        m.wr32(out + o + 4, (t >> 32) as u32);
    }
    let size = meta.map_or(0, |m| m.len());
    m.wr32(out + 28, (size >> 32) as u32);
    m.wr32(out + 32, size as u32);
    put_cstr(out + 44, &e.file_name().to_string_lossy(), 260);
    objects().add(Obj::Heap)
}

/// `*` and `?` against a name, without regard to case.
fn wildcard(pat: &str, name: &str) -> bool {
    fn m(p: &[u8], n: &[u8]) -> bool {
        match (p.first(), n.first()) {
            (None, None) => true,
            (Some(b'*'), _) => m(&p[1..], n) || !n.is_empty() && m(p, &n[1..]),
            (Some(b'?'), Some(_)) => m(&p[1..], &n[1..]),
            (Some(a), Some(b)) => a.eq_ignore_ascii_case(b) && m(&p[1..], &n[1..]),
            _ => false,
        }
    }
    let pat = if pat == "*.*" { "*" } else { pat };
    m(pat.as_bytes(), name.as_bytes())
}

// ---- file times ---------------------------------------------------------------------------------

/// A FILETIME (100 ns since 1601) of a host time.
fn file_time(t: std::time::SystemTime) -> u64 {
    let d = t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    d.as_secs() * 10_000_000 + d.subsec_nanos() as u64 / 100 + 116_444_736_000_000_000
}

fn get_file_time(c: &mut Cpu) -> u32 {
    let Some(f) = file_of(arg(c, 0)) else { return 0 };
    let times = f.lock().unwrap().times();
    for (k, t) in times.into_iter().enumerate() {
        let p = arg(c, 1 + k as u32);
        if p != 0 {
            let v = t.map_or(0, file_time);
            mem().wr32(p, v as u32);
            mem().wr32(p + 4, (v >> 32) as u32);
        }
    }
    1
}

/// SYSTEMTIME fields of a FILETIME.
fn system_time_of(ft: u64) -> [u16; 8] {
    let ms = ft / 10_000;
    let (days, rest) = ((ms / 86_400_000) as i64 - 134_774, ms % 86_400_000); // days since 1970
    let (year, month, day) = civil(days);
    let weekday = (days + 4).rem_euclid(7); // 1970-01-01 was a Thursday
    let s = rest / 1000;
    [year as u16, month as u16, weekday as u16, day as u16, (s / 3600) as u16, (s / 60 % 60) as u16, (s % 60) as u16, (rest % 1000) as u16]
}

/// A FILETIME of SYSTEMTIME fields.
fn file_time_of(st: [u16; 8]) -> u64 {
    let (y, mo, d) = (st[0] as i64, st[1] as i64, st[3] as i64);
    // days since 1970-01-01 from a civil date (Howard Hinnant's algorithm)
    let y = if mo <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if mo > 2 { mo - 3 } else { mo + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    let ms = ((days + 134_774) * 86_400_000) as u64 + st[4] as u64 * 3_600_000 + st[5] as u64 * 60_000 + st[6] as u64 * 1000 + st[7] as u64;
    ms * 10_000
}

/// The civil date (year, month, day) of days since 1970-01-01 (Howard Hinnant's algorithm).
fn civil(days: i64) -> (i64, i64, i64) {
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + (month <= 2) as i64, month, day)
}

fn rd_file_time(p: u32) -> u64 {
    mem().rd32(p) as u64 | (mem().rd32(p + 4) as u64) << 32
}

fn wr_system_time(p: u32, st: [u16; 8]) {
    for (k, v) in st.into_iter().enumerate() {
        mem().wr16(p + 2 * k as u32, v);
    }
}

fn file_time_to_system_time(c: &mut Cpu) -> u32 {
    wr_system_time(arg(c, 1), system_time_of(rd_file_time(arg(c, 0))));
    1
}

fn system_time_to_file_time(c: &mut Cpu) -> u32 {
    let p = arg(c, 0);
    let st: [u16; 8] = std::array::from_fn(|k| mem().rd16(p + 2 * k as u32));
    let ft = file_time_of(st);
    mem().wr32(arg(c, 1), ft as u32);
    mem().wr32(arg(c, 1) + 4, (ft >> 32) as u32);
    1
}

/// FileTimeToLocalFileTime: local time is UTC (as GetTimeZoneInformation says).
fn file_time_to_local_file_time(c: &mut Cpu) -> u32 {
    let ft = rd_file_time(arg(c, 0));
    mem().wr32(arg(c, 1), ft as u32);
    mem().wr32(arg(c, 1) + 4, (ft >> 32) as u32);
    1
}

/// GetSystemTime and GetLocalTime: the time in UTC (as the time zone says).
fn get_system_time(c: &mut Cpu) -> u32 {
    wr_system_time(arg(c, 0), system_time_of(file_time(std::time::SystemTime::now())));
    0
}

// ---- misc ---------------------------------------------------------------------------------------

fn interlocked_exchange(c: &mut Cpu) -> u32 {
    let (p, v) = (arg(c, 0), arg(c, 1));
    let old = mem().rd32(p);
    mem().wr32(p, v);
    old
}
fn get_time_zone_information(c: &mut Cpu) -> u32 {
    mem().slice(arg(c, 0), 172).fill(0);
    0 // TIME_ZONE_ID_UNKNOWN: UTC
}

/// GlobalMemoryStatus(MEMORYSTATUS): 512 MB of memory, half of it free, as a PC of 2002.
fn global_memory_status(c: &mut Cpu) -> u32 {
    let p = arg(c, 0);
    let m = mem();
    for (k, v) in [32u32, 50, 512 << 20, 256 << 20, 1024 << 20, 768 << 20, 0x7ffe_0000, 0x6000_0000].into_iter().enumerate() {
        m.wr32(p + 4 * k as u32, v);
    }
    0
}

fn get_environment_variable(c: &mut Cpu) -> u32 {
    if super::trace_api() {
        log!("GetEnvironmentVariableA {}: none", cstr(arg(c, 0)));
    }
    set_last_error(203); // ERROR_ENVVAR_NOT_FOUND
    0
}

fn get_user_default_lcid(_: &mut Cpu) -> u32 {
    0x0407 // German (Germany): the game is the German version
}

fn mul_div(c: &mut Cpu) -> u32 {
    let (a, b, d) = (arg(c, 0) as i32 as i64, arg(c, 1) as i32 as i64, arg(c, 2) as i32 as i64);
    if d == 0 {
        return u32::MAX;
    }
    // rounded half away from zero
    let n = a * b;
    let q = (n.abs() + d.abs() / 2) / d.abs();
    let q = if (n < 0) != (d < 0) { -q } else { q };
    i32::try_from(q).map_or(u32::MAX, |q| q as u32)
}

/// IsProcessorFeaturePresent: no MMX, SSE or 3DNow! (see `rt` cpuid: the lifter has none of
/// them), no Pentium errata.
fn is_processor_feature_present(c: &mut Cpu) -> u32 {
    if super::trace_api() {
        log!("IsProcessorFeaturePresent({}): no", arg(c, 0));
    }
    0
}

fn lstrcmpi(c: &mut Cpu) -> u32 {
    let (a, b) = (cstr(arg(c, 0)).to_lowercase(), cstr(arg(c, 1)).to_lowercase());
    a.cmp(&b) as i32 as u32
}

pub const IMPORTS: &[Import] = &[
    ("FindResourceA", 3, find_resource),
    ("LoadResource", 2, load_resource),
    ("LockResource", 1, first_arg),
    ("_lopen", 2, lopen),
    ("_lread", 3, lread),
    ("_lclose", 1, lclose),
    ("GetFileSize", 2, get_file_size),
    ("DeleteFileA", 1, delete_file),
    ("SetCurrentDirectoryA", 1, set_current_directory),
    ("InterlockedExchange", 2, interlocked_exchange),
    ("SetUnhandledExceptionFilter", 1, ret0),
    ("IsBadReadPtr", 2, ret0),
    ("IsBadWritePtr", 2, ret0),
    ("IsBadCodePtr", 1, ret0),
    ("GetTimeZoneInformation", 1, get_time_zone_information),
    ("GetLocalTime", 1, get_system_time),
    ("GetSystemTime", 1, get_system_time),
    ("FileTimeToSystemTime", 2, file_time_to_system_time),
    ("SystemTimeToFileTime", 2, system_time_to_file_time),
    ("FileTimeToLocalFileTime", 2, file_time_to_local_file_time),
    ("GetFileTime", 4, get_file_time),
    ("SetFileTime", 4, ret1),
    ("GetCurrentDirectoryA", 2, get_current_directory),
    ("GetFullPathNameA", 4, get_full_path_name),
    ("GetFileAttributesA", 1, get_file_attributes),
    ("CreateDirectoryA", 2, create_directory),
    ("MoveFileA", 2, move_file),
    ("FindFirstFileA", 2, find_first_file),
    ("GlobalMemoryStatus", 1, global_memory_status),
    ("GetEnvironmentVariableA", 3, get_environment_variable),
    ("GetUserDefaultLCID", 0, get_user_default_lcid),
    ("IsValidCodePage", 1, ret1),
    ("IsValidLocale", 2, ret1),
    ("MulDiv", 3, mul_div),
    ("IsProcessorFeaturePresent", 1, is_processor_feature_present),
    ("lstrcmpiA", 2, lstrcmpi),
    ("GetVersionExA", 1, get_version_ex),
    ("TlsFree", 1, ret1),
    ("GetVersion", 0, get_version),
    ("GetCommandLineA", 0, get_command_line),
    ("GetStartupInfoA", 1, get_startup_info),
    ("GetModuleHandleA", 1, get_module_handle),
    ("GetModuleFileNameA", 3, get_module_file_name),
    ("GetEnvironmentStrings", 0, get_environment_strings),
    ("GetEnvironmentStringsW", 0, get_environment_strings),
    ("FreeEnvironmentStringsA", 1, ret1),
    ("FreeEnvironmentStringsW", 1, ret1),
    ("SetEnvironmentVariableA", 2, ret1),
    ("SetEnvironmentVariableW", 2, ret1),
    ("ExitProcess", 1, exit_process),
    ("TerminateProcess", 2, exit_process_from_terminate),
    ("ExitThread", 1, exit_thread),
    ("GetSystemInfo", 1, get_system_info),
    ("GetComputerNameA", 2, get_computer_name),
    ("OutputDebugStringA", 1, output_debug_string),
    ("RaiseException", 4, raise_exception),
    ("RtlUnwind", 4, ret0),
    ("UnhandledExceptionFilter", 1, ret0),
    ("DebugBreak", 0, ret0),
    ("Beep", 2, ret1),
    ("SetErrorMode", 1, ret0),
    ("GetLastError", 0, get_last_error),
    ("SetLastError", 1, set_last_error_imp),
    ("GetCurrentProcess", 0, get_current_process),
    ("GetCurrentThread", 0, get_current_thread),
    ("GetCurrentThreadId", 0, get_current_thread_id),
    ("SetHandleCount", 1, first_arg),
    ("SetStdHandle", 2, ret1),
    ("LoadLibraryA", 1, load_library),
    ("FreeLibrary", 1, ret1),
    ("GetProcAddress", 2, get_proc_address),
    ("GetLogicalDrives", 0, get_logical_drives),
    ("GetDriveTypeA", 1, get_drive_type),
    ("GetVolumeInformationA", 8, get_volume_information),
    ("GetACP", 0, get_acp),
    ("GetOEMCP", 0, get_oemcp),
    ("GetCPInfo", 2, get_cp_info),
    ("MultiByteToWideChar", 6, multi_byte_to_wide_char),
    ("WideCharToMultiByte", 8, wide_char_to_multi_byte),
    ("GetStringTypeA", 5, get_string_type_a),
    ("GetStringTypeW", 4, get_string_type_w),
    ("LCMapStringA", 6, lc_map_string_a),
    ("LCMapStringW", 6, lc_map_string_w),
    ("CompareStringA", 6, compare_string_a),
    ("CompareStringW", 6, compare_string_w),
    ("GetLocaleInfoA", 4, get_locale_info_a),
    ("GetLocaleInfoW", 4, get_locale_info_w),
    ("lstrlenA", 1, lstrlen),
    ("lstrcpyA", 2, lstrcpy),
    ("HeapCreate", 3, heap_create),
    ("HeapDestroy", 1, ret1),
    ("HeapAlloc", 3, heap_alloc),
    ("HeapFree", 3, heap_free),
    ("HeapReAlloc", 4, heap_realloc),
    ("HeapSize", 3, heap_size),
    ("VirtualAlloc", 4, virtual_alloc),
    ("VirtualFree", 3, virtual_free),
    ("VirtualProtect", 4, virtual_protect),
    ("GlobalAlloc", 2, global_alloc),
    ("GlobalFree", 1, global_free),
    ("GlobalLock", 1, global_same),
    ("GlobalHandle", 1, global_same),
    ("GlobalUnlock", 1, global_unlock),
    ("TlsAlloc", 0, tls_alloc),
    ("TlsGetValue", 1, tls_get_value),
    ("TlsSetValue", 2, tls_set_value),
    ("InitializeCriticalSection", 1, init_cs),
    ("DeleteCriticalSection", 1, delete_cs),
    ("EnterCriticalSection", 1, enter_cs),
    ("LeaveCriticalSection", 1, leave_cs),
    ("InterlockedIncrement", 1, interlocked_increment),
    ("InterlockedDecrement", 1, interlocked_decrement),
    ("CreateEventA", 4, create_event),
    ("SetEvent", 1, set_event),
    ("ResetEvent", 1, reset_event),
    ("PulseEvent", 1, pulse_event),
    ("CreateMutexA", 3, create_mutex),
    ("WaitForSingleObject", 2, wait_for_single_object),
    ("WaitForMultipleObjects", 4, wait_for_multiple_objects),
    ("WaitForMultipleObjectsEx", 5, wait_for_multiple_objects),
    ("CloseHandle", 1, close_handle),
    ("DuplicateHandle", 7, duplicate_handle),
    ("CreateThread", 6, create_thread),
    ("ResumeThread", 1, resume_thread),
    ("TerminateThread", 2, terminate_thread),
    ("SetThreadPriority", 2, ret1),
    ("GetThreadPriority", 1, ret0),
    ("GetTickCount", 0, get_tick_count),
    ("QueryPerformanceFrequency", 1, query_performance_frequency),
    ("QueryPerformanceCounter", 1, query_performance_counter),
    ("Sleep", 1, sleep),
    ("SleepEx", 2, sleep),
    ("CreateFileA", 7, create_file),
    ("ReadFile", 5, read_file),
    ("WriteFile", 5, write_file),
    ("SetFilePointer", 4, set_file_pointer),
    ("SetEndOfFile", 1, set_end_of_file),
    ("FlushFileBuffers", 1, flush_file_buffers),
    ("GetStdHandle", 1, get_std_handle),
    ("GetFileType", 1, get_file_type),
];

fn exit_process_from_terminate(c: &mut Cpu) -> u32 {
    let code = arg(c, 1);
    super::advapi32::save();
    log!("TerminateProcess({code})");
    std::process::exit(code as i32)
}
