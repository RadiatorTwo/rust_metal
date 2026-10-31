//! The world's id router and a latent bug of the game when it quits during a mission's start.
//!
//! A world (current one: [0x72a458]->vt+0x14, its router at +0x2c) routes object ids to the
//! containers ("handlers") that own them: a binary tree at router+0x9084 keyed by the id's top
//! byte (node: handler, list, key, left +0xc, right +0x10; registered by 0x5053b0 from
//! 0x45a3c0, eleven of them). `ROUTE` (0x507890, router vt+0x44) finds the handler of an id
//! and calls handler->vt+0xc(id) to remove it.
//!
//! When the world is destroyed (0x413cb0 -> router dtor 0x503140), the router first removes
//! the ids on its lists (0x503440), then walks its tree in post-order (0x5095a0) and deletes
//! every node's handler (`DROP_NODE`, 0x503040), each container deleting its objects. An
//! object of type 2 (vtable 0x5e4bc8, item dtor 0x41db70) holds the id of another object at
//! +0x1d0+0x28 and removes it through `ROUTE` (0x4374b0) when it is deleted. If that other
//! object's container was deleted earlier in the walk, `ROUTE` finds the deleted container
//! (its vtable already the abstract base 0x5e45fc) and the call ends in _purecall: R6025.
//! In a running mission the lists hold those ids and nothing is left for the walk; while a
//! mission is starting (after the briefing's "BEREIT", before its music) they are not on the
//! lists yet, so closing the game then (WM_CLOSE: Alt+F4 on the original, our window's close
//! button or a script's `close`) crashes the original too.
//!
//! The fix keeps the game's logic: while a router is being destroyed, `ROUTE` to a handler
//! that router has already deleted does nothing and says "not found" (what it says for an id
//! without a handler); the object it would remove went with its container.

use super::{log, mem};
use crate::rt::{Cpu, EAX, ECX, ESP, call_original};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

/// thiscall(router, id) -> al: remove the object `id` through its type's handler.
pub const ROUTE: u32 = 0x507890;
/// thiscall(router, node): delete a tree node's handler (the router dtor's walk).
pub const DROP_NODE: u32 = 0x503040;
/// thiscall(router): the router's destructor.
pub const ROUTER_DTOR: u32 = 0x503140;

/// The abstract handler base's vtable (every slot _purecall but the dtor).
const ABSTRACT_VTABLE: u32 = 0x5e45fc;

/// (router, handler) pairs: handlers deleted by a router destructor still running.
static DEAD: Mutex<Vec<(u32, u32)>> = Mutex::new(Vec::new());
/// Whether DEAD is not empty (`ROUTE` is called often; it looks only then).
static ANY_DEAD: AtomicBool = AtomicBool::new(false);

fn trace() -> bool {
    static T: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *T.get_or_init(|| std::env::var_os("GUNMETAL_TRACE_ROUTER").is_some())
}

/// The handler a router's tree holds for `id` (what 0x507890 finds), if any. `rd` reads a
/// guest dword. Tree node: handler +0, key +8, left +0xc (smaller keys), right +0x10.
pub fn handler_of(rd: &dyn Fn(u32) -> u32, router: u32, id: u32) -> Option<u32> {
    let key = id & 0xff00_0000;
    let mut node = rd(router + 0x9084);
    // a tree of eleven nodes; the bound only guards against a broken one
    for _ in 0..256 {
        if node == 0 {
            return None;
        }
        let k = rd(node + 8);
        if k == key {
            return Some(rd(node));
        }
        node = rd(node + if key > k { 0x10 } else { 0xc });
    }
    None
}

pub fn route(c: &mut Cpu) {
    if ANY_DEAD.load(Ordering::Relaxed) {
        let m = mem();
        let (router, id) = (c.r[ECX], m.rd32(c.r[ESP] + 4));
        if let Some(h) = handler_of(&|a| m.rd32(a), router, id) {
            if super::lock(&DEAD).contains(&(router, h)) {
                let vt = m.rd32(h);
                log!(
                    "game: id {id:#x} removed from router {router:#x} after its handler {h:#x} (vtable {vt:#x}{}) was deleted: skipped",
                    if vt == ABSTRACT_VTABLE { ", the abstract base" } else { "" }
                );
                // ret 4 with al = 0 (not found)
                c.r[EAX] &= !0xff;
                c.r[ESP] += 8;
                return;
            }
        }
    }
    call_original(c, ROUTE);
}

pub fn drop_node(c: &mut Cpu) {
    let m = mem();
    let (router, node) = (c.r[ECX], m.rd32(c.r[ESP] + 4));
    let h = m.rd32(node);
    if trace() {
        let vt = if h != 0 { m.rd32(h) } else { 0 };
        log!("router {router:#x}: deleting handler {h:#x} (key {:#x}, vtable {vt:#x})", m.rd32(node + 8));
    }
    call_original(c, DROP_NODE);
    if h != 0 {
        super::lock(&DEAD).push((router, h));
        ANY_DEAD.store(true, Ordering::Relaxed);
    }
}

pub fn router_dtor(c: &mut Cpu) {
    let router = c.r[ECX];
    if trace() {
        log!("router {router:#x}: destroyed");
    }
    call_original(c, ROUTER_DTOR);
    let mut d = super::lock(&DEAD);
    d.retain(|e| e.0 != router);
    ANY_DEAD.store(!d.is_empty(), Ordering::Relaxed);
}

pub const HOOKS: [(u32, crate::rt::Hook); 3] = [(ROUTE, route), (DROP_NODE, drop_node), (ROUTER_DTOR, router_dtor)];
