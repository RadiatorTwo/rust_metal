//! Reading the Ghidra export.

use rust_metal::ghidra::read;

#[test]
fn reads_export() {
    let f = read(r#"[{"addr": "0x401000", "size": 466, "name": "FUN_00401000", "thunk": false, "chunks": 1}, {"addr": "0x5851a0", "size": 8, "name": "__ftol", "thunk": true, "chunks": 1}]"#);
    assert_eq!(f.len(), 2);
    assert_eq!(f[0].addr, 0x401000);
    assert_eq!(f[0].size, 466);
    assert!(!f[0].thunk);
    assert_eq!(f[1].name, "__ftol");
    assert!(f[1].thunk);
}

#[test]
fn embedded_export_has_every_line() {
    let json = include_str!("../data/gunmetal_functions.json");
    let objects = json.lines().filter(|l| l.starts_with('{')).count();
    assert!(objects > 1000);
    assert_eq!(rust_metal::ghidra::functions().len(), objects);
}
