//! Guest paths to host paths: the game names files in any case ("data\frontend\x.wmv"), the
//! folder on disk has its own ("data/FrontEnd/X.wmv"). Needs no game files.

use rust_metal::platform::resolve_case;
use std::path::PathBuf;

/// A new empty folder below the system's temp folder, with spaces, `!` and a non-ASCII letter.
fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("rust_metal_paths_{tag}_{}", std::process::id())).join("a !b ü");
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn finds_mixed_case() {
    let dir = temp_dir("mixed");
    let real = dir.join("data").join("FrontEnd");
    std::fs::create_dir_all(&real).unwrap();
    std::fs::write(real.join("Attract_ps.wmv"), b"x").unwrap();
    let found = resolve_case(&dir, "data\\frontend\\attract_ps.wmv");
    assert_eq!(found, Some(real.join("Attract_ps.wmv")));
    let _ = std::fs::remove_dir_all(dir.parent().unwrap());
}

#[test]
fn missing_is_none() {
    let dir = temp_dir("missing");
    std::fs::create_dir_all(dir.join("data")).unwrap();
    assert_eq!(resolve_case(&dir, "data\\nothing.pck"), None);
    assert_eq!(resolve_case(&dir, "nothing\\x.pck"), None);
    let _ = std::fs::remove_dir_all(dir.parent().unwrap());
}
