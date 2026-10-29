//! DSOUND: DirectSoundCreate8 (ordinal 11, the game's only DirectSound import). Not
//! implemented yet: the game is told there is no sound device.

use super::{Import, arg, log, mem};
use crate::rt::Cpu;

const DSERR_NODRIVER: u32 = 0x8878_0078;

/// DirectSoundCreate8(device, out, outer).
fn direct_sound_create8(c: &mut Cpu) -> u32 {
    log!("warn: DirectSoundCreate8 is not implemented (no sound device)");
    if arg(c, 1) != 0 {
        mem().wr32(arg(c, 1), 0);
    }
    DSERR_NODRIVER
}

pub const IMPORTS: &[Import] = &[("#11", 3, direct_sound_create8)];
