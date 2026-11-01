//! Guest COM objects for host interfaces. Later tasks pass the game's Direct3D and DirectSound
//! calls through to the system's DLLs; the game must hold guest objects whose vtables are our
//! thunks, and each thunk finds the host interface behind `this`. A host pointer is 64 bits, so
//! a guest object keeps an index into a host-side table instead. The table also maps host
//! pointer -> guest object: D3D9 hands out the same interface again (GetBackBuffer every frame),
//! which must not allocate a new guest object each time.
//!
//! Reference counts stay the host's: a guest AddRef/Release thunk calls the host's and then
//! `release(this, count)` with what it returned. At 0 the guest object goes away with its shadows.
//! A shadow is a guest buffer kept per (object, key) for data that the game reads or writes by
//! guest address (locked surfaces and buffers) and the host sees through a copy.

use super::{Import, alloc, com_new, free, lock, mem};
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A guest buffer of `size` bytes.
#[derive(Clone, Copy)]
pub struct Shadow {
    pub guest: u32,
    pub size: u32,
}

struct Entry {
    host: usize,
    guest: u32,
    shadows: HashMap<u32, Shadow>,
    /// host pointers of objects that live and die with this one (`adopt`)
    children: Vec<usize>,
}

#[derive(Default)]
struct Table {
    entries: Vec<Option<Entry>>,
    vacant: Vec<u32>,
    by_host: HashMap<usize, u32>,
}

static TABLE: Mutex<Option<Table>> = Mutex::new(None);

/// The host pointers of the first `HOSTS.len()` table entries (0 for none), for `host` without
/// `TABLE`'s lock: it runs at every COM call, ~1 million a second in a mission.
static HOSTS: [AtomicUsize; 1 << 16] = [const { AtomicUsize::new(0) }; 1 << 16];

fn with<T>(f: impl FnOnce(&mut Table) -> T) -> T {
    f(lock(&TABLE).get_or_insert_with(Table::default))
}

/// The guest object for a host interface pointer; the same pointer gives the same object while
/// it lives.
///
/// An entry can outlive its host object: a surface of a texture answers Release with the
/// texture's count, so its entry stays when the game lets go of it, and when the texture goes
/// the surface goes with it unseen. A new host object at the same address then finds that
/// entry. Of another interface (seen: a vertex buffer created where a texture's surface had
/// been, then locked through the surface's vtable, a crash at mission starts), the old entry
/// is dropped (`retire`) and the new object gets its own.
pub fn wrap(methods: &'static [Import], host: *mut c_void) -> u32 {
    let key = host as usize;
    let vt = super::com_vtable(methods);
    if let Some(g) = with(|t| hit(t, key, vt)) {
        return g;
    }
    let guest = com_new(methods, 4);
    with(|t| {
        // another thread may have wrapped it meanwhile
        if let Some(g) = hit(t, key, vt) {
            free(guest);
            return g;
        }
        let e = Entry { host: key, guest, shadows: HashMap::new(), children: Vec::new() };
        let i = match t.vacant.pop() {
            Some(i) => {
                t.entries[i as usize] = Some(e);
                i
            }
            None => {
                t.entries.push(Some(e));
                t.entries.len() as u32 - 1
            }
        };
        t.by_host.insert(key, i);
        if let Some(h) = HOSTS.get(i as usize) {
            h.store(key, Ordering::Release);
        }
        mem().wr32(guest + 4, i);
        guest
    })
}

/// The live guest object of vtable `vt` for host pointer `key`; one of another vtable there is
/// stale and retired.
fn hit(t: &mut Table, key: usize, vt: u32) -> Option<u32> {
    let i = *t.by_host.get(&key)?;
    let g = t.entries[i as usize].as_ref().unwrap().guest;
    if mem().rd32(g) == vt {
        return Some(g);
    }
    let was = super::thunk_index(mem().rd32(mem().rd32(g))).map(super::thunk_name).unwrap_or_default();
    let now = super::thunk_index(mem().rd32(vt)).map(super::thunk_name).unwrap_or_default();
    super::log!(
        "com: host object {key:#x} is new ({}), its address was a released {}'s: that guest object {g:#x} is retired",
        now.split("::").next().unwrap_or("?"),
        was.split("::").next().unwrap_or("?")
    );
    let e = take(t, i as usize);
    retire(e);
    None
}

fn index(this: u32) -> usize {
    mem().rd32(this + 4) as usize
}

/// The host interface pointer behind a guest object.
pub fn host(this: u32) -> *mut c_void {
    let i = index(this);
    if let Some(h) = HOSTS.get(i).map(|h| h.load(Ordering::Acquire)).filter(|&h| h != 0) {
        return h as *mut c_void;
    }
    with(|t| t.entries[index(this)].as_ref().expect("COM call on a released object").host as *mut c_void)
}

/// Released guest objects are kept this long (in releases) before their memory is freed: a
/// call through a stale pointer reaches `dead_call` instead of an object that reuses the block.
const QUARANTINE: usize = 4096;
static DEAD: Mutex<std::collections::VecDeque<u32>> = Mutex::new(std::collections::VecDeque::new());

/// The vtable of released objects: every method is `dead_call`.
fn dead_vtable() -> u32 {
    static V: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *V.get_or_init(|| {
        let n = 128;
        let v = alloc(4 * n);
        for k in 0..n {
            mem().wr32(v + 4 * k, super::thunk(&format!("released COM object, method {k}"), 0, dead_call));
        }
        v
    })
}

/// A method called on a released object (the game kept a pointer after its last Release):
/// reported with the interface it was and the caller's stack, then the run ends.
fn dead_call(c: &mut crate::rt::Cpu) -> u32 {
    use crate::rt::ESP;
    let this = super::arg(c, 0);
    let was = mem().rd32(this + 4);
    let name = super::thunk_index(was).map(super::thunk_name).unwrap_or_default();
    let iface = name.split("::").next().unwrap_or("?");
    super::log!("fatal: a method of the released COM object {this:#x} (was {iface}) called from {:#x}", mem().rd32(c.r[ESP]));
    for line in super::crash::stack_here(c) {
        super::log!("{line}");
    }
    std::process::exit(101)
}

/// After the host's Release returned `host_count`: at 0 free the guest object, its shadows and
/// the table entry (the object's own block after a quarantine, `QUARANTINE`).
pub fn release(this: u32, host_count: u32) {
    if host_count != 0 {
        return;
    }
    let i = index(this);
    let gone = with(|t| {
        let e = take(t, i);
        // its surfaces went with it (their Release answered with its count, never 0)
        let kids: Vec<u32> = e.children.iter().filter_map(|k| t.by_host.get(k).copied()).collect();
        let mut gone: Vec<Entry> = kids.into_iter().map(|c| take(t, c as usize)).collect();
        gone.push(e);
        gone
    });
    for e in gone {
        retire(e);
    }
}

/// Entry `i` out of the table.
fn take(t: &mut Table, i: usize) -> Entry {
    let e = t.entries[i].take().expect("COM object released twice");
    if let Some(h) = HOSTS.get(i) {
        h.store(0, Ordering::Release);
    }
    t.by_host.remove(&e.host);
    t.vacant.push(i as u32);
    e
}

/// `child` (a texture's surface) lives and dies with `parent`: D3D9 forwards a surface's
/// AddRef and Release to its texture, so the surface's own Release never answers 0.
pub fn adopt(child: u32, parent: u32) {
    let key = host(child) as usize;
    with(|t| {
        if let Some(e) = t.entries[index(parent)].as_mut() {
            if !e.children.contains(&key) {
                e.children.push(key);
            }
        }
    });
}

/// A guest object whose host object is gone: its shadows freed, the object itself given the
/// dead vtable (at +4, where the index was, its old vtable's first thunk for the report) and
/// freed after the quarantine.
fn retire(e: Entry) {
    for s in e.shadows.values() {
        free(s.guest);
    }
    let this = e.guest;
    let old = mem().rd32(mem().rd32(this));
    mem().wr32(this + 4, old);
    mem().wr32(this, dead_vtable());
    let mut dead = lock(&DEAD);
    dead.push_back(this);
    if dead.len() > QUARANTINE {
        free(dead.pop_front().unwrap());
    }
}

/// Every shadow's guest range [start, end) (for the frame log's page findings).
pub fn shadow_ranges() -> Vec<(u32, u32)> {
    with(|t| t.entries.iter().flatten().flat_map(|e| e.shadows.values().map(|s| (s.guest, s.guest + s.size))).collect())
}

/// Every shadow with its owner: [start, end) and "interface key size" (the interface's name
/// from its vtable's first thunk), for the frame log's page findings.
pub fn shadow_owners() -> Vec<(u32, u32, String)> {
    let objs: Vec<(u32, Vec<(u32, Shadow)>)> = with(|t| t.entries.iter().flatten().map(|e| (e.guest, e.shadows.iter().map(|(k, s)| (*k, *s)).collect())).collect());
    let mut out = Vec::new();
    for (guest, shadows) in objs {
        let first = mem().rd32(mem().rd32(guest));
        let name = super::thunk_index(first).map(super::thunk_name).unwrap_or_default();
        let iface = name.split("::").next().unwrap_or("?").to_string();
        for (k, s) in shadows {
            out.push((s.guest, s.guest + s.size, format!("{iface} key {k:#x} size {}", s.size)));
        }
    }
    out
}

/// The guest buffer for (object, key) of at least `size` bytes: the same one while it is big
/// enough, a new one (the old freed, contents not kept) when a bigger size is asked.
pub fn shadow(this: u32, key: u32, size: u32) -> u32 {
    let i = index(this);
    let (old, kept) = with(|t| match t.entries[i].as_ref().expect("COM call on a released object").shadows.get(&key) {
        Some(s) if s.size >= size => (None, Some(s.guest)),
        Some(s) => (Some(s.guest), None),
        None => (None, None),
    });
    if let Some(g) = kept {
        return g;
    }
    if let Some(g) = old {
        free(g);
    }
    let guest = alloc(size);
    with(|t| t.entries[i].as_mut().unwrap().shadows.insert(key, Shadow { guest, size }));
    guest
}
