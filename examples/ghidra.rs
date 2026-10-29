//! Compares our function discovery with a Ghidra export:  ghidra <game folder> [export.json]
//! Reads the export (default data/gunmetal_functions.json; written by a Ghidra script: one
//! object per line) and lists the functions Ghidra found that we do not have as an entry,
//! those we have that Ghidra does not, and the names Ghidra gave (other than FUN_...) that
//! data/symbols.txt does not have yet.
//!
//! Note: the game's seeds already contain every Ghidra function (`Game::seeds`), so "Ghidra's,
//! not ours" can only show functions discovery drops; `GUNMETAL_NO_GHIDRA=1` seeds with the
//! entry point alone, which compares Ghidra with our own discovery.
use rust_metal::discover::{Code, Program, find_functions};
use rust_metal::game::Game;
use rust_metal::ghidra;
use std::collections::BTreeMap;
use std::path::Path;

fn main() {
    let dir = std::env::args().nth(1).expect("path to the game folder");
    let file = std::env::args().nth(2).unwrap_or_else(|| "data/gunmetal_functions.json".into());
    let g = ghidra::read(&std::fs::read_to_string(&file).unwrap_or_else(|e| panic!("{file}: {e}")));
    let game = Game::load(Path::new(&dir)).unwrap();
    let mut code = Code::new(&game.all);
    let ranges = if std::env::var_os("GUNMETAL_NO_GHIDRA").is_some() {
        find_functions(&mut code, &[(game.exe.entry, "entry".to_string())])
    } else {
        game.functions(&mut code)
    };
    let prog = Program::build(&mut code, &ranges);
    // every instruction we decode, for "inside one of ours"
    let decoded: std::collections::HashSet<u32> = prog.fns.values().flat_map(|f| f.ins.iter().copied()).collect();
    let known = rust_metal::image::symbols();
    let ours: BTreeMap<u32, &str> = ranges.iter().map(|(lo, _, n)| (*lo, n.as_str())).collect();
    let gset: std::collections::HashSet<u32> = g.iter().map(|f| f.addr).collect();
    let missing: Vec<_> = g.iter().filter(|f| !ours.contains_key(&f.addr) && !prog.fns.contains_key(&f.addr)).collect();
    let extra: Vec<_> = ours.keys().filter(|a| !gset.contains(a)).collect();
    println!("{file}: Ghidra {} functions, ours {}", g.len(), ours.len());
    println!("  Ghidra's, not an entry of ours: {} ({} of them are instructions we decode inside a function)", missing.len(), missing.iter().filter(|f| decoded.contains(&f.addr)).count());
    for f in missing.iter().filter(|f| !decoded.contains(&f.addr)).take(40) {
        println!("    {:#x} {} bytes {}", f.addr, f.size, f.name);
    }
    // inside a Ghidra function (we split it, or it is a second entry) or outside all
    let gstart: BTreeMap<u32, u32> = g.iter().map(|f| (f.addr, f.size)).collect();
    let inside = |a: u32| gstart.range(..a).next_back().is_some_and(|(s, n)| a < s + n);
    let (ins, out): (Vec<u32>, Vec<u32>) = extra.iter().map(|a| **a).partition(|a| inside(*a));
    println!("  ours, not Ghidra's: {} ({} inside a Ghidra function, {} outside all)", extra.len(), ins.len(), out.len());
    if std::env::args().any(|a| a == "-v") {
        for a in ins.iter().take(15) {
            let (s, n) = gstart.range(..*a).next_back().unwrap();
            println!("    {a:#x} inside {s:#x} ({n} bytes)");
        }
        for a in out.iter().take(15) {
            println!("    {a:#x} outside");
        }
    }
    let names: Vec<_> = g.iter().filter(|f| !f.name.starts_with("FUN_") && !f.name.starts_with("thunk_FUN_") && !known.contains_key(&f.addr)).collect();
    println!("  names not in symbols.txt: {}", names.len());
    for f in names {
        println!("    {:#x} {}", f.addr, f.name);
    }
}
