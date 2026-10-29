//! Reading a Ghidra function export (data/gunmetal_functions.json, written by a Ghidra
//! script): a JSON array with one object per line,
//! `{"addr": "0x401000", "size": 466, "name": "FUN_00401000", "thunk": false, "chunks": 1}`.

/// One function of the export.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GhidraFn {
    pub addr: u32,
    pub size: u32,
    pub name: String,
    pub thunk: bool,
}

/// The value of `"key": ...` in one line: a string without its quotes, or the bare token.
fn field(line: &str, key: &str) -> Option<String> {
    let rest = &line[line.find(&format!("\"{key}\": "))? + key.len() + 4..];
    Some(match rest.strip_prefix('"') {
        Some(s) => s[..s.find('"')?].to_string(),
        None => rest[..rest.find([',', '}'])?].trim().to_string(),
    })
}

/// Every function of an export; lines that are not function objects are skipped.
pub fn read(json: &str) -> Vec<GhidraFn> {
    json.lines()
        .flat_map(|l| l.split("}, {"))
        .filter_map(|l| {
            let addr = u32::from_str_radix(field(l, "addr")?.trim_start_matches("0x"), 16).ok()?;
            Some(GhidraFn { addr, size: field(l, "size")?.parse().ok()?, name: field(l, "name")?, thunk: field(l, "thunk")? == "true" })
        })
        .collect()
}

/// The functions of the export built into the program.
pub fn functions() -> Vec<GhidraFn> {
    read(include_str!("../data/gunmetal_functions.json"))
}
