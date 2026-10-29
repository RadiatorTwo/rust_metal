//! Gun Metal (2002) recompiled to native code with Cranelift.
//!
//! The player's Gunmetal.exe is decoded (iced-x86), every function is lifted to Cranelift IR
//! and compiled to host code; game pointers stay 32-bit in a 4 GB guest address space.

pub mod discover;
pub mod ghidra;
pub mod game;
pub mod image;
pub mod iso;
pub mod jit;
pub mod lift;
pub mod platform;
pub mod rt;
pub mod smc;
