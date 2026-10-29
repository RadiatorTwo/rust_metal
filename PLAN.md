# Plan — what is done, what is open

Updated 2026-10-29. Details on how things work: CLAUDE.md. Design:
`docs/superpowers/specs/2026-10-29-rust-metal-design.md`.

## Done

- [x] Recompiler core from rustschweine: image, discovery, x86 -> Cranelift, x87, SMC fields,
      runtime, JIT with cache, CD image reading; tests without game files
- [x] `Gunmetal.exe` loaded after a SHA-256 check; function discovery seeded with the
      entry point and the Ghidra export (`data/gunmetal_functions.json`, 6904 functions):
      9512 functions, 9793 entries, all compile (149 entries with MMX/3DNow!/SSE parts as
      stubs)
- [x] Discovery against Ghidra without its seeds (`GUNMETAL_NO_GHIDRA=1`): of 6904 Ghidra
      functions 6886 are found. Fixed: a lone `fwait` made `plausible` reject real functions
      (3 found). The 18 left are unreferenced: C runtime helpers called through tables
      (`__cintrindisp1`, `__ctrandisp1/2`, `__trandisp1`, `__fload`, 0x58a399, 0x58ae33),
      0x5bfbc9 (a 3DNow! D3DX routine, 4436 bytes) and 0x5cbe0a (an exception funclet);
      nothing in the code or data points at them. With the Ghidra seeds none is missing.

## Open

- [ ] Platform layer (Win32, DirectSound, DirectShow, D3D9 pass-through), `main.rs` runs the
      game (now it stops after compiling)
- [ ] MMX/3DNow!/SSE in the lifter, or CPUID answers that keep D3DX on lifted paths
      (see the design: the builder decides after seeing the code)
- [ ] Names for `data/symbols.txt` (Ghidra has 1192 non-`FUN_` names: C runtime, MFC, D3DX)
