//! Where flags cross calls, returns and jumps between functions:  flags <game folder>
//! (`discover::FlagCrossings`: the lifter stores and loads flags only there).
use rust_metal::discover::{Code, Program};
use rust_metal::game::Game;
use std::path::Path;

fn main() {
    let dir = std::env::args().nth(1).expect("path to the game folder");
    let game = Game::load(Path::new(&dir)).unwrap();
    let mut code = Code::new(&game.all);
    let ranges = game.functions(&mut code);
    let prog = Program::build(&mut code, &ranges);
    let x = &prog.flags;
    let sorted = |s: &std::collections::HashSet<u32>| {
        let mut v: Vec<u32> = s.iter().copied().collect();
        v.sort_unstable();
        v
    };
    println!("{} entries read flags at entry: {:x?}", x.read_at_entry.len(), sorted(&x.read_at_entry));
    println!("{} calls read flags after: {:x?}", x.read_after_call.len(), sorted(&x.read_after_call));
    println!("{} functions return flags: {:x?}", x.returned.len(), sorted(&x.returned));
}
