//! The crash report's stack walk (`crash::ebp_chain`, `crash::return_scan`) on made-up stacks
//! and code. Needs no game files.

use rust_metal::platform::crash::{ebp_chain, looks_like_return, return_scan};
use std::collections::HashMap;

/// A made-up guest memory: dwords by address.
fn stack(words: &[(u32, u32)]) -> impl Fn(u32) -> Option<u32> {
    let m: HashMap<u32, u32> = words.iter().copied().collect();
    move |a| m.get(&a).copied()
}

#[test]
fn walks_frames_until_the_chain_leaves_the_stack() {
    // three frames; the last one's saved ebp points outside the stack
    let rd = stack(&[(0x1100, 0x1180), (0x1104, 0x401234), (0x1180, 0x1300), (0x1184, 0x405678), (0x1300, 0x9000), (0x1304, 0x40abcd)]);
    assert_eq!(ebp_chain(&rd, 0x1100, 0x1000, 0x2000, 16), vec![0x401234, 0x405678, 0x40abcd]);
}

#[test]
fn stops_at_a_loop_a_backward_link_or_a_misaligned_frame() {
    // saved ebp equal to the frame (a loop)
    let rd = stack(&[(0x1100, 0x1100), (0x1104, 0x401234)]);
    assert_eq!(ebp_chain(&rd, 0x1100, 0x1000, 0x2000, 16), vec![0x401234]);
    // a link to a lower address (not a caller's frame)
    let rd = stack(&[(0x1100, 0x1180), (0x1104, 0x401234), (0x1180, 0x1080), (0x1184, 0x405678), (0x1080, 0x1200), (0x1084, 0x4)]);
    assert_eq!(ebp_chain(&rd, 0x1100, 0x1000, 0x2000, 16), vec![0x401234, 0x405678]);
    // misaligned
    let rd = stack(&[(0x1100, 0x1182), (0x1104, 0x401234)]);
    assert_eq!(ebp_chain(&rd, 0x1100, 0x1000, 0x2000, 16), vec![0x401234]);
}

#[test]
fn nothing_outside_the_stack_or_beyond_the_limit() {
    let rd = stack(&[(0x1100, 0x1180), (0x1104, 0x401234), (0x1180, 0x1200), (0x1184, 0x405678)]);
    assert!(ebp_chain(&rd, 0x900, 0x1000, 0x2000, 16).is_empty());
    assert!(ebp_chain(&rd, 0x2000, 0x1000, 0x2000, 16).is_empty());
    // the frame's return address must be inside the stack too
    assert!(ebp_chain(&rd, 0x1ffc, 0x1000, 0x2000, 16).is_empty());
    assert_eq!(ebp_chain(&rd, 0x1100, 0x1000, 0x2000, 1), vec![0x401234]);
    // unreadable memory ends it
    assert_eq!(ebp_chain(&rd, 0x1180, 0x1000, 0x2000, 16), vec![0x405678]);
    // a return address of 0 ends it (the thread's first frame)
    let rd = stack(&[(0x1100, 0x1180), (0x1104, 0)]);
    assert!(ebp_chain(&rd, 0x1100, 0x1000, 0x2000, 16).is_empty());
}

/// Made-up code bytes by address.
fn code(bytes: &[(u32, &[u8])]) -> impl Fn(u32) -> Option<u8> {
    let mut m = HashMap::new();
    for &(a, b) in bytes {
        for (i, &x) in b.iter().enumerate() {
            m.insert(a + i as u32, x);
        }
    }
    move |a| m.get(&a).copied()
}

#[test]
fn return_addresses_follow_a_call() {
    let rd8 = code(&[
        (0x401000, &[0xe8, 1, 2, 3, 4]),             // call rel32
        (0x401010, &[0xff, 0xd0]),                   // call eax
        (0x401020, &[0xff, 0x15, 0, 0x10, 0x64, 0]), // call [abs]
        (0x401030, &[0xff, 0x50, 0x3c]),             // call [eax+3c]
        (0x401040, &[0xff, 0x90, 0, 1, 0, 0]),       // call [eax+100]
        (0x401050, &[0x8b, 0xc1, 0x90, 0x90, 0x90]), // no call
        (0x401060, &[0xff, 0xe0]),                   // jmp eax
    ]);
    for a in [0x401005, 0x401012, 0x401026, 0x401033, 0x401046] {
        assert!(looks_like_return(&rd8, a), "{a:#x}");
    }
    for a in [0x401055, 0x401062, 0x401001] {
        assert!(!looks_like_return(&rd8, a), "{a:#x}");
    }
}

#[test]
fn the_scan_lists_code_addresses_after_calls() {
    let rd8 = code(&[(0x401000, &[0xe8, 1, 2, 3, 4]), (0x401010, &[0xff, 0xd0]), (0x401050, &[0x8b, 0xc1, 0x90])]);
    let rd = stack(&[(0x1000, 0x401005), (0x1004, 0x12345), (0x1008, 0x401053), (0x100c, 0x401012), (0x1010, 0x401005)]);
    let text = 0x401000..0x5e3315;
    assert_eq!(return_scan(&rd, &rd8, 0x1000, 0x2000, text.clone(), 64, 16), vec![(0x1000, 0x401005), (0x100c, 0x401012), (0x1010, 0x401005)]);
    assert_eq!(return_scan(&rd, &rd8, 0x1000, 0x2000, text.clone(), 64, 2), vec![(0x1000, 0x401005), (0x100c, 0x401012)]);
    // only `words` dwords from esp, never past the stack's end
    assert_eq!(return_scan(&rd, &rd8, 0x1000, 0x2000, text.clone(), 3, 16), vec![(0x1000, 0x401005)]);
    assert_eq!(return_scan(&rd, &rd8, 0x1000, 0x1008, text, 64, 16), vec![(0x1000, 0x401005)]);
}
