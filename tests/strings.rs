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

/// repne scasb, then the flags to the scratch's last dword
fn repne_scasb(a: &mut CodeAssembler) -> Result<(), IcedError> {
    args(a)?;
    a.repne().scasb()?;
    a.pushfd()?;
    a.pop(edx)?;
    a.mov(dword_ptr(SCRATCH + 0xffc), edx)?;
    finish(a)
}

/// What the CPU does for repne scasb forwards: (edi, ecx, flags CF PF ZF SF OF)
fn scasb_reference(mem: &[u8], mut dst: u32, mut cnt: u32, byte: u8) -> (u32, u32, Option<u32>) {
    let mut flags = None;
    while cnt != 0 {
        let (x, y) = (byte as u32, mem[(dst - SCRATCH) as usize] as u32);
        let r = x.wrapping_sub(y) & 0xff;
        let f = (x < y) as u32 | (((r as u8).count_ones() % 2 == 0) as u32) << 2 | ((r == 0) as u32) << 6 | ((r >> 7) & 1) << 7 | ((((x ^ y) & (x ^ r)) >> 7) & 1) << 11;
        flags = Some(f);
        dst += 1;
        cnt -= 1;
        if r == 0 {
            break;
        }
    }
    (dst, cnt, flags)
}

#[test]
fn repne_scas() {
    let g = Guest::new(&[("repne_scasb", repne_scasb as Snippet)]);
    let mask = 1 | 4 | 0x40 | 0x80 | 0x800;
    // (start, count, byte): found, not found within the count, count 0, found as the last
    // byte counted, strlen (count 0xffffffff), a byte above 0x7f
    let s = SCRATCH + 0x100;
    let cases = [(s, 100, 0x55u8), (s, 3, 0x55), (s, 0, 0x55), (SCRATCH + 0xf00, 0xf0, 0), (s, u32::MAX, 0), (s, 50, 0xc3)];
    for &(dst, cnt, byte) in &cases {
        let mut fill: Vec<u8> = (0..0x1000u32).map(|i| (i * 7 + 3) as u8 | 1).collect();
        fill[0x140] = 0x55;
        fill[0x110] = 0xc3;
        fill[0xfff] = 0;
        fill[0x180] = 0;
        g.mem.slice(SCRATCH, 0xff0).copy_from_slice(&fill[..0xff0]);
        // the last byte of the case at 0xf00 (below the results at the scratch's end)
        g.mem.slice(SCRATCH + 0xfef, 1)[0] = 0;
        let mem = g.mem.slice(SCRATCH, 0x1000).to_vec();
        let (wdst, wcnt, wflags) = scasb_reference(&mem, dst, cnt, byte);
        g.call(0, &[0, dst, cnt, byte as u32, 0]);
        let what = format!("dst {dst:#x} cnt {cnt:#x} byte {byte:#x}");
        assert_eq!((g.mem.rd32(SCRATCH + 0xff4), g.mem.rd32(SCRATCH + 0xff8)), (wdst, wcnt), "registers: {what}");
        if let Some(f) = wflags {
            assert_eq!(g.mem.rd32(SCRATCH + 0xffc) & mask, f, "flags: {what}");
        }
    }
}
