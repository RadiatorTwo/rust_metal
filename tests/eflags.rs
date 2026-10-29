//! EFLAGS bits without an effect that programs toggle to learn about the CPU: ID (bit 21,
//! "CPUID exists", the game's test at 0x5633f0) and AC (bit 18, "a 486 or later") are kept
//! by popfd and seen by pushfd. Needs no game files.

mod common;

use common::{Guest, Snippet};
use iced_x86::IcedError;
use iced_x86::code_asm::*;

/// (mask): eax = the flags after popfd of pushfd's value with `mask` toggled, ecx = before
fn toggle(a: &mut CodeAssembler) -> Result<(), IcedError> {
    a.mov(edx, dword_ptr(esp + 4))?;
    a.pushfd()?;
    a.pop(ecx)?;
    a.push(ecx)?;
    a.xor(dword_ptr(esp), edx)?;
    a.popfd()?;
    a.pushfd()?;
    a.pop(eax)?;
    // put the flags back as they were
    a.push(ecx)?;
    a.popfd()?;
    a.ret()
}

const SNIPPETS: [(&str, Snippet); 1] = [("toggle", toggle)];

#[test]
fn id_and_ac_toggle() {
    let g = Guest::new(&SNIPPETS);
    for bit in [21, 18] {
        let cpu = g.call(0, &[1 << bit]);
        let (before, after) = (cpu.r[1], cpu.r[0]);
        assert_ne!(before & 1 << bit, after & 1 << bit, "bit {bit} did not toggle: {before:#x} -> {after:#x}");
        // and back again
        let cpu = g.call(0, &[0]);
        assert_eq!(cpu.r[1] & 1 << bit, cpu.r[0] & 1 << bit);
    }
}
