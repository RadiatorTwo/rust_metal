//! Stand-in for the platform layer (Win32 and Direct3D 9 in Rust), which comes later: only
//! what the recompiler core refers to.

use crate::rt::Hook;

/// Game functions replaced by our own (`jit::compile`): none yet.
pub const HOOKS: &[(u32, Hook)] = &[];

/// A NUL-terminated string in guest memory (needs the guest memory, which only the platform
/// layer will set up).
pub fn cstr(_a: u32) -> String {
    unimplemented!("platform layer not implemented yet")
}
