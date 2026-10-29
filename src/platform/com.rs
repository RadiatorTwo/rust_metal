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
}

#[derive(Default)]
struct Table {
    entries: Vec<Option<Entry>>,
    vacant: Vec<u32>,
    by_host: HashMap<usize, u32>,
}

static TABLE: Mutex<Option<Table>> = Mutex::new(None);

fn with<T>(f: impl FnOnce(&mut Table) -> T) -> T {
    f(lock(&TABLE).get_or_insert_with(Table::default))
}

/// The guest object for a host interface pointer; the same pointer gives the same object while
/// it lives.
pub fn wrap(methods: &'static [Import], host: *mut c_void) -> u32 {
    let key = host as usize;
    if let Some(g) = with(|t| t.by_host.get(&key).map(|&i| t.entries[i as usize].as_ref().unwrap().guest)) {
        return g;
    }
    let guest = com_new(methods, 4);
    with(|t| {
        // another thread may have wrapped it meanwhile
        if let Some(&i) = t.by_host.get(&key) {
            free(guest);
            return t.entries[i as usize].as_ref().unwrap().guest;
        }
        let e = Entry { host: key, guest, shadows: HashMap::new() };
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
        mem().wr32(guest + 4, i);
        guest
    })
}

fn index(this: u32) -> usize {
    mem().rd32(this + 4) as usize
}

/// The host interface pointer behind a guest object.
pub fn host(this: u32) -> *mut c_void {
    with(|t| t.entries[index(this)].as_ref().expect("COM call on a released object").host as *mut c_void)
}

/// After the host's Release returned `host_count`: at 0 free the guest object, its shadows and
/// the table entry.
pub fn release(this: u32, host_count: u32) {
    if host_count != 0 {
        return;
    }
    let i = index(this);
    let e = with(|t| {
        let e = t.entries[i].take().expect("COM object released twice");
        t.by_host.remove(&e.host);
        t.vacant.push(i as u32);
        e
    });
    for s in e.shadows.values() {
        free(s.guest);
    }
    free(this);
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
