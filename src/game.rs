//! The game's program: Gunmetal.exe (32-bit x86, base 0x400000, statically linked C runtime,
//! D3DX9 and the game code in one file). Only the one build with `SHA256` is supported.

use crate::discover::Code;
use crate::image::Image;
use sha2::{Digest, Sha256};
use std::io::{Error, ErrorKind};
use std::path::Path;

pub const EXE: &str = "Gunmetal.exe";

/// The supported Gunmetal.exe.
pub const SHA256: [u8; 32] = [
    0xae, 0xd2, 0x6f, 0xfb, 0xf8, 0x00, 0xda, 0x41, 0xc7, 0x41, 0xe5, 0xd4, 0x92, 0xa0, 0x1f, 0xc1, 0xe4, 0x9d, 0xa2, 0xa5, 0x46, 0x72, 0xc7, 0x0d, 0xce, 0x48, 0xa9, 0x3f, 0xe8, 0xcc, 0xe6, 0xe4,
];

pub struct Game {
    pub exe: Image,
    /// the program as the image for finding and compiling the code (the EXE alone)
    pub all: Image,
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

impl Game {
    /// From the game's folder; fails with a message if Gunmetal.exe is not the supported build.
    pub fn load(dir: &Path) -> std::io::Result<Game> {
        let path = dir.join(EXE);
        let bytes = std::fs::read(&path).map_err(|e| Error::new(e.kind(), format!("{}: {e}", path.display())))?;
        let sum = Sha256::digest(&bytes);
        if sum[..] != SHA256 {
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!("{} is not the supported {EXE} (SHA-256 {}, found {})", path.display(), hex(&SHA256), hex(&sum)),
            ));
        }
        let exe = Image::load(&path)?;
        let all = Image::combine(&[&exe]);
        Ok(Game { exe, all })
    }

    /// Where finding the functions starts: the entry point and every function of the Ghidra
    /// export, with the names Ghidra gave them (not `FUN_...`).
    pub fn seeds(&self) -> Vec<(u32, String)> {
        let mut s = vec![(self.exe.entry, "entry".to_string())];
        for f in crate::ghidra::functions() {
            let name = if f.name.starts_with("FUN_") || f.name.starts_with("thunk_FUN_") { format!("sub_{:x}", f.addr) } else { f.name };
            s.push((f.addr, name));
        }
        s
    }

    /// Every function: sorted (start, end, name).
    pub fn functions(&self, code: &mut Code) -> Vec<(u32, u32, String)> {
        crate::discover::find_functions(code, &self.seeds())
    }
}
