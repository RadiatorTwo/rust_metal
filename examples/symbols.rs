//! Writes the named functions of a Ghidra export as data/symbols.txt:  symbols <export.json>
//! Every function not called FUN_..., thunk_FUN_... or an Unwind@/Catch@ funclet, grouped by name pattern (C runtime,
//! MFC, D3DX9, other); the descriptions are for filling in by hand.
use rust_metal::ghidra;

fn group(name: &str) -> usize {
    let mfc = name.starts_with('C') && name.chars().nth(1).is_some_and(|c| c.is_ascii_uppercase());
    if name.starts_with('_') || name.starts_with("operator_") || name.starts_with("`") || name.starts_with('~') {
        0
    } else if mfc {
        1
    } else if name.starts_with("D3DX") || name.starts_with("d3dx") {
        2
    } else {
        3
    }
}

fn main() {
    let file = std::env::args().nth(1).expect("path to the export (data/gunmetal_functions.json)");
    let mut fns = ghidra::read(&std::fs::read_to_string(&file).unwrap_or_else(|e| panic!("{file}: {e}")));
    fns.retain(|f| !["FUN_", "thunk_FUN_", "Unwind@", "Catch@"].iter().any(|p| f.name.starts_with(p)));
    fns.sort_by_key(|f| f.addr);
    println!("# What we know about the game's functions and data: address, name, description (\"?\" marks");
    println!("# guesses). Gunmetal.exe at 0x400000. Names from the Ghidra export; descriptions by hand.");
    for (i, title) in ["C runtime", "MFC", "D3DX9", "other"].iter().enumerate() {
        println!("\n# {title}");
        for f in fns.iter().filter(|f| group(&f.name) == i) {
            println!("{:#x} {}", f.addr, f.name);
        }
    }
}
