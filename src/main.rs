//! Gun Metal recompiled: compiles the player's Gunmetal.exe and runs it.
//!
//!     gunmetal [game folder]
//!
//! Without a path: `game_files` in the current folder.

use rust_metal::discover::{Code, Program};
use rust_metal::game::Game;
use rust_metal::jit::compile_cached;
use rust_metal::smc::Smc;
use sha2::{Digest, Sha256};
use std::path::PathBuf;

/// mimalloc: the Direct3D worker frees what the game's thread allocates for every recorded
/// call (~4000 a picture); with the system heap that cost ~0.1 us a call.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;
use std::time::Instant;

fn main() {
    // guest calls nest on the host stack like the original's: run on a thread with a large one
    let code = std::thread::Builder::new().stack_size(256 << 20).spawn(run).unwrap().join().unwrap();
    std::process::exit(code);
}

fn run() -> i32 {
    let dir = PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| "game_files".into()));
    let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
    let game = match Game::load(&dir) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let t = Instant::now();
    let mut code = Code::new(&game.all);
    let ranges = game.functions(&mut code);
    let prog = Program::build(&mut code, &ranges);
    let smc = Smc::find(&prog, &mut code);
    let found = t.elapsed().as_millis();
    // the machine code is kept in the game folder, for this program and this file
    let cache = dir.join("compiled.bin");
    let hooks = rust_metal::platform::hooks();
    // the tick thread needs the pages generated code writes (rt::marking)
    rust_metal::rt::set_marking(rust_metal::platform::world::mode(&dir) != rust_metal::platform::world::Mode::Off);
    let key = cache_key(&game, &hooks);
    let compiled = compile_cached(&prog, &smc, &mut code, &hooks, key.map(|k| (cache.as_path(), k)));
    let st = &compiled.stats;
    if st.cached {
        eprintln!("compiled code of {} functions loaded in {} ms", st.functions, t.elapsed().as_millis());
    } else {
        eprintln!(
            "compiled {} functions in {} ms (finding them {found}, lifting {}, compiling {})",
            st.functions,
            t.elapsed().as_millis(),
            st.lift_ms,
            st.compile_ms
        );
    }
    let code = rust_metal::platform::run_game(&game, dir);
    drop(compiled);
    code as i32
}

/// What the compiled code depends on: this program (any change to the recompiler) and the
/// game's file. None when our own file cannot be read.
fn cache_key(game: &Game, hooks: &[(u32, rust_metal::rt::Hook)]) -> Option<[u8; 32]> {
    let own = std::fs::read(std::env::current_exe().ok()?).ok()?;
    let mut h = Sha256::new();
    h.update(&own);
    h.update(&game.all.data);
    // which functions are hooked changes the compiled code (GUNMETAL_TRACE_LIFE)
    for (a, _) in hooks {
        h.update(a.to_le_bytes());
    }
    h.update([rust_metal::rt::marking() as u8]);
    Some(h.finalize().into())
}
