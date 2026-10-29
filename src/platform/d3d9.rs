//! d3d9.dll, which the game loads with LoadLibraryA (`d3d9.dll`, else `d3d9d.dll`) and asks
//! for `Direct3DCreate9` (0x469530, the first thing the game does after the C runtime, its CPU
//! detection and CoInitialize, before its window exists). Direct3D 9 is not implemented yet:
//! the run ends at that call.

use super::{Import, arg, log, mem};
use crate::rt::Cpu;

/// The CPU's clock rate the game measured (cpu_mhz, 0x563350).
const CPU_MHZ: u32 = 0x76c474;

/// Direct3DCreate9(SDK version): where start-up ends for now.
fn direct3d_create9(c: &mut Cpu) -> u32 {
    log!("Direct3DCreate9 reached (SDK version {:#x}, the game measured {} MHz); Direct3D 9 is not implemented yet", arg(c, 0), mem().rd32(CPU_MHZ));
    super::advapi32::save();
    std::process::exit(0)
}

pub const IMPORTS: &[Import] = &[("Direct3DCreate9", 1, direct3d_create9)];
