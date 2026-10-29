//! ole32: COM start-up and CoCreateInstance, which finds the classes we implement by CLSID
//! (`register_class`: DirectShow's filter graph and the like, by the modules that implement
//! them); task memory from the guest heap.

use super::{Import, alloc, arg, free, log, mem};
use crate::rt::Cpu;
use std::sync::Mutex;

pub const REGDB_E_CLASSNOTREG: u32 = 0x8004_0154;

/// Makes an object of a class for interface `iid` (guest address of the IID), writes it to
/// `out` and returns the HRESULT.
pub type Create = fn(iid: u32, out: u32) -> u32;

static CLASSES: Mutex<Vec<([u8; 16], Create)>> = Mutex::new(Vec::new());

/// Make a class known to CoCreateInstance (a later registration of the same CLSID wins).
pub fn register_class(clsid: [u8; 16], create: Create) {
    let mut c = CLASSES.lock().unwrap_or_else(|e| e.into_inner());
    c.retain(|e| e.0 != clsid);
    c.push((clsid, create));
}

/// A GUID in its usual spelling, {01234567-89ab-cdef-0123-456789abcdef}.
pub fn guid_string(g: &[u8; 16]) -> String {
    let d1 = u32::from_le_bytes(g[..4].try_into().unwrap());
    let d2 = u16::from_le_bytes([g[4], g[5]]);
    let d3 = u16::from_le_bytes([g[6], g[7]]);
    let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
    format!("{{{d1:08x}-{d2:04x}-{d3:04x}-{}-{}}}", hex(&g[8..10]), hex(&g[10..]))
}

/// A GUID from guest memory.
pub fn guid_at(p: u32) -> [u8; 16] {
    mem().slice(p, 16).try_into().unwrap()
}

/// CoCreateInstance(clsid, outer, context, iid, out).
fn co_create_instance(c: &mut Cpu) -> u32 {
    let (clsid, iid, out) = (guid_at(arg(c, 0)), arg(c, 3), arg(c, 4));
    let create = CLASSES.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|e| e.0 == clsid).map(|e| e.1);
    match create {
        Some(f) => f(iid, out),
        None => {
            log!("CoCreateInstance({}, iid {}): class not registered", guid_string(&clsid), guid_string(&guid_at(iid)));
            if out != 0 {
                mem().wr32(out, 0);
            }
            REGDB_E_CLASSNOTREG
        }
    }
}

fn co_task_mem_alloc(c: &mut Cpu) -> u32 {
    alloc(arg(c, 0))
}

fn co_task_mem_free(c: &mut Cpu) -> u32 {
    free(arg(c, 0));
    0
}

fn ret0(_: &mut Cpu) -> u32 {
    0
}

pub const IMPORTS: &[Import] = &[
    ("CoInitialize", 1, ret0),
    ("CoUninitialize", 0, ret0),
    ("CoCreateInstance", 5, co_create_instance),
    ("CoTaskMemAlloc", 1, co_task_mem_alloc),
    ("CoTaskMemFree", 1, co_task_mem_free),
    ("CoFreeUnusedLibraries", 0, ret0),
];
