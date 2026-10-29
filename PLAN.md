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
- [x] Platform base from rustschweine (kernel32, user32, gdi32, advapi32, winmm, ole32 with
      `register_class`, window with winit and no GL): the game runs from the C runtime
      start-up to Direct3DCreate9, where the run ends (`d3d9.rs`). Paths case-insensitive
      (`resolve_case`, test `tests/paths.rs`)
- [x] CPUID answered as a Pentium Pro without MMX/SSE/3DNow!: D3DX9 stays on its x87 paths;
      no stub entry runs during start-up
- [x] CD check understood (0x459240: a CD-ROM drive with gmsetup\data2.cab over 52 MB) and
      answered: D: is a CD-ROM, its files the game folder's, and what the game folder lacks
      comes from `GunMetal.iso` (test `tests/iso.rs`); seen passing with a stand-in
      IDirect3D9. Registry: the game reads none of its own

## Open

- [ ] Direct3D 9 pass-through (`d3d9.rs`): Direct3DCreate9, then the window's HWND
      (`window::host_hwnd`) for CreateDevice. The game sizes its window to the desktop
      (GetSystemMetrics: `user32::DESKTOP_W/H`, 1600x1200 now) and makes it WS_POPUP; the
      winit window keeps the size of CreateWindowExA (320x240) so far
- [ ] DirectSound (`dsound.rs`: DirectSoundCreate8 says "no device" now), DirectShow
      (`ole32::register_class`)
- [ ] The game's own CPU speed is 0 (no CPUID seen: pushfd/popfd lose the ID flag), so a
      first start picks the slow-CPU default of a detail setting (0x459904); decide whether
      to report a speed
- [ ] Imports bound to "not implemented" (stop when called): CreateProcessA,
      EnumSystemLocalesA, CreateFileW, CreateFileMappingA, MapViewOfFile, UnmapViewOfFile
- [ ] MMX/3DNow!/SSE in the lifter: not needed while CPUID hides them
- [ ] Names for `data/symbols.txt` (Ghidra has 1192 non-`FUN_` names: C runtime, MFC, D3DX)
