//! movs and stos, with and without rep: short forward ones run in generated code, the others
//! through `rt::rt_string`; both must give what the CPU gives (registers and memory, an
//! overlapping copy repeating what it wrote). Needs no game files.

mod common;

use common::{Guest, SCRATCH, Snippet};
use iced_x86::IcedError;
use iced_x86::code_asm::*;

/// (esi, edi, ecx, eax, df)
fn args(a: &mut CodeAssembler) -> Result<(), IcedError> {
    a.push(esi)?;
    a.push(edi)?;
    a.mov(esi, dword_ptr(esp + 12))?;
    a.mov(edi, dword_ptr(esp + 16))?;
    a.mov(ecx, dword_ptr(esp + 20))?;
    a.mov(eax, dword_ptr(esp + 24))?;
    a.cmp(dword_ptr(esp + 28), 0)?;
    let mut fwd = a.create_label();
    a.je(fwd)?;
    a.std()?;
    a.set_label(&mut fwd)?;
    Ok(())
}

/// After the string instruction: esi, edi, ecx to the scratch's end, registers back.
fn finish(a: &mut CodeAssembler) -> Result<(), IcedError> {
    a.cld()?;
    a.mov(dword_ptr(SCRATCH + 0xff0), esi)?;
    a.mov(dword_ptr(SCRATCH + 0xff4), edi)?;
    a.mov(dword_ptr(SCRATCH + 0xff8), ecx)?;
    a.pop(edi)?;
    a.pop(esi)?;
    a.ret()
}

macro_rules! snippet {
    ($name:ident, $($ins:tt)*) => {
        fn $name(a: &mut CodeAssembler) -> Result<(), IcedError> {
            args(a)?;
            a.$($ins)*;
            finish(a)
        }
    };
}
snippet!(rep_movsd, rep().movsd()?);
snippet!(rep_movsb, rep().movsb()?);
snippet!(rep_stosd, rep().stosd()?);
snippet!(rep_stosw, rep().stosw()?);
snippet!(movsb1, movsb()?);
snippet!(stosd1, stosd()?);

const SNIPPETS: [(&str, Snippet); 6] = [
    ("rep_movsd", rep_movsd),
    ("rep_movsb", rep_movsb),
    ("rep_stosd", rep_stosd),
    ("rep_stosw", rep_stosw),
    ("movsb1", movsb1),
    ("stosd1", stosd1),
];

/// What the CPU does: (kind 0 movs / 1 stos, size, rep) on `mem` (offsets from SCRATCH).
fn reference(mem: &mut [u8], kind: usize, size: u32, rep: bool, mut src: u32, mut dst: u32, mut cnt: u32, val: u32, df: bool) -> (u32, u32, u32) {
    let step = if df { size.wrapping_neg() } else { size };
    loop {
        if rep && cnt == 0 {
            break;
        }
        // the whole element read, then written
        let v: Vec<u8> = (0..size).map(|b| if kind == 0 { mem[(src - SCRATCH + b) as usize] } else { (val >> (8 * b)) as u8 }).collect();
        for b in 0..size {
            mem[(dst - SCRATCH + b) as usize] = v[b as usize];
        }
        if kind == 0 {
            src = src.wrapping_add(step);
        }
        dst = dst.wrapping_add(step);
        if !rep {
            break;
        }
        cnt -= 1;
    }
    (src, dst, cnt)
}

#[test]
fn movs_and_stos() {
    let g = Guest::new(&SNIPPETS);
    let kinds = [(0, 4, true), (0, 1, true), (1, 4, true), (1, 2, true), (0, 1, false), (1, 4, false)];
    let s = SCRATCH + 0x100;
    // (source, destination, count, df): short, empty, long (the helper), overlapping both
    // ways, backwards
    let cases = [(s, s + 0x400, 16, false), (s, s + 0x400, 0, false), (s, s + 0x400, 200, false), (s, s + 1, 40, false), (s + 3, s, 40, false), (s + 0x200, s + 0x300, 10, true)];
    for (k, &(kind, size, rep)) in kinds.iter().enumerate() {
        for &(src, dst, cnt, df) in &cases {
            let fill: Vec<u8> = (0..0xf00u32).map(|i| (i * 7 + 3) as u8).collect();
            g.mem.slice(SCRATCH, 0xf00).copy_from_slice(&fill);
            let mut want = fill.clone();
            let val = 0x1234_5678;
            let (wsrc, wdst, wcnt) = reference(&mut want, kind, size, rep, src, dst, cnt, val, df);
            let cpu = g.call(k, &[src, dst, cnt, val, df as u32]);
            let got = (g.mem.rd32(SCRATCH + 0xff0), g.mem.rd32(SCRATCH + 0xff4), g.mem.rd32(SCRATCH + 0xff8));
            let what = format!("{} src {src:#x} dst {dst:#x} cnt {cnt} df {df}", SNIPPETS[k].0);
            assert_eq!(got, (if kind == 0 { wsrc } else { src }, wdst, wcnt), "registers: {what}");
            assert_eq!(cpu.r[0], val, "eax: {what}");
            assert!(g.mem.slice(SCRATCH, 0xf00) == &want[..], "memory: {what}");
        }
    }
}
