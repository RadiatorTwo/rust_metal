//! Reading a CD image: a tiny ISO 9660 image made here (a label, a folder, a file over two
//! sectors), its files found by path and read in place. Needs no game files.

use rust_metal::iso::Iso;
use std::io::{Read, Seek, SeekFrom};

/// A directory record: name, first sector, size, folder or not.
fn record(name: &[u8], sector: u32, size: u32, dir: bool) -> Vec<u8> {
    let len = 33 + name.len() + (name.len() + 1) % 2;
    let mut r = vec![0u8; len];
    r[0] = len as u8;
    r[2..6].copy_from_slice(&sector.to_le_bytes());
    r[10..14].copy_from_slice(&size.to_le_bytes());
    r[25] = if dir { 2 } else { 0 };
    r[32] = name.len() as u8;
    r[33..33 + name.len()].copy_from_slice(name);
    r
}

/// Sectors 16 (primary descriptor, label "GUNMETAL"), 17 (end), 18 (root: GMSETUP), 19
/// (GMSETUP: DATA2.CAB), 20-21 (the file, 3000 bytes).
fn image() -> (Vec<u8>, Vec<u8>) {
    let mut d = vec![0u8; 22 * 2048];
    let s = |n: usize| n * 2048;
    d[s(16)] = 1;
    d[s(16) + 1..s(16) + 6].copy_from_slice(b"CD001");
    d[s(16) + 40..s(16) + 72].copy_from_slice(b"GUNMETAL                        ");
    let root = record(&[0], 18, 2048, true);
    d[s(16) + 156..s(16) + 156 + root.len()].copy_from_slice(&root);
    d[s(17)] = 0xff;
    d[s(17) + 1..s(17) + 6].copy_from_slice(b"CD001");
    let dirs = [(18, vec![record(&[0], 18, 2048, true), record(&[1], 18, 2048, true), record(b"GMSETUP", 19, 2048, true)]), (19, vec![record(&[0], 19, 2048, true), record(&[1], 18, 2048, true), record(b"DATA2.CAB;1", 20, 3000, false)])];
    for (sector, recs) in dirs {
        let mut at = s(sector);
        for r in recs {
            d[at..at + r.len()].copy_from_slice(&r);
            at += r.len();
        }
    }
    let file: Vec<u8> = (0..3000u32).map(|k| (k * 7 % 251) as u8).collect();
    d[s(20)..s(20) + 3000].copy_from_slice(&file);
    (d, file)
}

#[test]
fn reads_a_file_in_place() {
    let (img, file) = image();
    let path = std::env::temp_dir().join(format!("rust_metal_iso_{}.iso", std::process::id()));
    std::fs::write(&path, &img).unwrap();
    let mut iso = Iso::open(&path).unwrap();
    assert_eq!(iso.label, "GUNMETAL");
    assert!(iso.lookup("gmsetup\\nothing.cab").is_none());
    let e = iso.lookup("gmsetup\\data2.cab").unwrap();
    assert_eq!(e.size, 3000);
    let mut f = iso.open_file(&e).unwrap();
    // what the game's CD check does: 1024 bytes before the end, 128 bytes
    assert_eq!(f.seek(SeekFrom::End(-1024)).unwrap(), 3000 - 1024);
    let mut buf = [0u8; 128];
    f.read_exact(&mut buf).unwrap();
    assert_eq!(&buf[..], &file[3000 - 1024..3000 - 1024 + 128]);
    // across the sector boundary, and to the end
    f.seek(SeekFrom::Start(2000)).unwrap();
    let mut rest = Vec::new();
    f.read_to_end(&mut rest).unwrap();
    assert_eq!(rest, file[2000..]);
    drop(f);
    let _ = std::fs::remove_file(&path);
}
