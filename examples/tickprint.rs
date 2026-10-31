//! Compares two logic fingerprint files (`GUNMETAL_TICK_PRINT`): the first tick and field
//! where they differ, and how many lines differ.
//!
//!     cargo run --release --example tickprint -- a.txt b.txt

use rust_metal::platform::ticks::first_divergence;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [a, b] = &args[..] else {
        eprintln!("usage: tickprint <a.txt> <b.txt>");
        std::process::exit(2);
    };
    let (ta, tb) = (std::fs::read_to_string(a).expect("first file"), std::fs::read_to_string(b).expect("second file"));
    let (la, lb): (Vec<&str>, Vec<&str>) = (ta.lines().collect(), tb.lines().collect());
    let n = la.len().min(lb.len());
    let differ = (0..n).filter(|&i| la[i] != lb[i]).count();
    println!("{} and {} lines; {differ} of the first {n} differ", la.len(), lb.len());
    match first_divergence(&la, &lb) {
        None => println!("identical"),
        Some((i, field)) => {
            println!("first difference at line {} in {field}:", i + 1);
            println!("  a: {}", la.get(i).unwrap_or(&"(none)"));
            println!("  b: {}", lb.get(i).unwrap_or(&"(none)"));
            // fields that differ from there on, counted
            let mut by: std::collections::BTreeMap<String, usize> = Default::default();
            for j in i..n {
                if la[j] != lb[j] {
                    for (x, y) in la[j].split_whitespace().zip(lb[j].split_whitespace()) {
                        if x != y {
                            *by.entry(x.split('=').next().unwrap_or("").to_string()).or_default() += 1;
                        }
                    }
                }
            }
            println!("differing lines by field: {by:?}");
        }
    }
}
