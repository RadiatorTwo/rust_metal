//! The router's tree lookup (`router::handler_of`) on a made-up tree. Needs no game files.

use rust_metal::platform::router::handler_of;
use std::collections::HashMap;

/// A router at 0x1000 whose tree (root at +0x9084) has keys 5, 2, 7 and 3 inserted in that
/// order (node: handler, list, key, left, right), handlers 0x5000.. by key.
fn tree() -> HashMap<u32, u32> {
    let mut m = HashMap::new();
    let mut node = |at: u32, key: u32, left: u32, right: u32| {
        m.insert(at, 0x5000 + key);
        m.insert(at + 8, key << 24);
        m.insert(at + 0xc, left);
        m.insert(at + 0x10, right);
    };
    node(0x2000, 5, 0x2100, 0x2200);
    node(0x2100, 2, 0, 0x2300);
    node(0x2200, 7, 0, 0);
    node(0x2300, 3, 0, 0);
    m.insert(0x1000 + 0x9084, 0x2000);
    m
}

#[test]
fn finds_the_handler_of_an_ids_top_byte() {
    let m = tree();
    let rd = |a: u32| m.get(&a).copied().unwrap_or(0);
    assert_eq!(handler_of(&rd, 0x1000, 0x0500_000d), Some(0x5005));
    assert_eq!(handler_of(&rd, 0x1000, 0x05ff_ffff), Some(0x5005));
    assert_eq!(handler_of(&rd, 0x1000, 0x0200_0001), Some(0x5002));
    assert_eq!(handler_of(&rd, 0x1000, 0x0300_0000), Some(0x5003));
    assert_eq!(handler_of(&rd, 0x1000, 0x0700_1234), Some(0x5007));
}

#[test]
fn an_id_without_a_handler_or_an_empty_tree_finds_none() {
    let m = tree();
    let rd = |a: u32| m.get(&a).copied().unwrap_or(0);
    assert_eq!(handler_of(&rd, 0x1000, 0x0400_0000), None);
    assert_eq!(handler_of(&rd, 0x1000, 0x0900_0000), None);
    assert_eq!(handler_of(&|_| 0, 0x1000, 0x0500_0000), None);
}

#[test]
fn a_tree_with_a_loop_ends() {
    // a node whose right child is itself
    let m: HashMap<u32, u32> = [(0x1000 + 0x9084, 0x2000), (0x2000, 0x5001), (0x2008, 0x0100_0000), (0x2010, 0x2000)].into_iter().collect();
    let rd = |a: u32| m.get(&a).copied().unwrap_or(0);
    assert_eq!(handler_of(&rd, 0x1000, 0x0900_0000), None);
}
