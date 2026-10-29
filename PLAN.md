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
      no stub entry runs during start-up. The game's own detection sees CPUID (EFLAGS ID kept
      by pushfd/popfd) and measures the real clock (rdtsc = host TSC: 3403 MHz here)
- [x] CD check understood (0x459240: a CD-ROM drive with gmsetup\data2.cab over 52 MB) and
      answered: D: is a CD-ROM, its files the game folder's, and what the game folder lacks
      comes from `GunMetal.iso` (test `tests/iso.rs`); seen passing with a stand-in
      IDirect3D9. Registry: the game reads none of its own

- [x] Direct3D 9 passed through (`d3d9/`): IDirect3D9, IDirect3DDevice9 (119 methods in SDK
      order), textures, texture surfaces, vertex/index buffers, queries; the game's device
      (1024x768 fullscreen, 2x MSAA) is created on the winit window; 16-bit display modes
      offered as the 32-bit ones (Windows 8+ has none; the game insists on two formats per
      resolution). The window follows SetWindowPos/WS_POPUP; GetSystemMetrics is the real
      display (tests `tests/d3d9_types.rs`)

## Open

- [ ] First frame: game_start needs DirectInput 7 (CoCreateInstance CLSID_DirectInput,
      IID_IDirectInput7W; input_init 0x4ddcc0) and then a DirectShow filter graph for the
      intro video before its first Present (Tasks 10 and 9 come before the first frame);
      Present, Clear and `GUNMETAL_SHOT` are in place but not yet seen running in the game
- [ ] Direct3D 9 still open: shaders, vertex declarations, render targets and other device
      surfaces, cube/volume textures, state blocks, GetAdapterMonitor
- [ ] DirectSound (`dsound.rs`: DirectSoundCreate8 says "no device" now), DirectShow
      (`ole32::register_class`)
- [ ] Imports bound to "not implemented" (stop when called): CreateProcessA,
      EnumSystemLocalesA, CreateFileW, CreateFileMappingA, MapViewOfFile, UnmapViewOfFile
- [ ] MMX/3DNow!/SSE in the lifter: not needed while CPUID hides them
- [ ] Names for `data/symbols.txt` (Ghidra has 1192 non-`FUN_` names: C runtime, MFC, D3DX)
