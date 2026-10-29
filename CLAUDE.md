# CLAUDE.md — Gun Metal in Rust

Hand-over for future sessions. What is done and what is open: **PLAN.md** (keep it current).
Design: `docs/superpowers/specs/2026-10-29-rust-metal-design.md`.

## Goal (the user's)

A native port of Gun Metal (Rage Games 2002, PC, version 1.09.1.1, German) in Rust, built like
the user's earlier ports of Future Cop (`C:\!Code2\future_rust_cop`) and Hogs of War
(`C:\!Code2\rustschweine`, also by the user): the game's own x86 code is recompiled with
Cranelift when the game starts, and the Windows/DirectX functions it calls are implemented in
Rust (Direct3D 9 is passed through to the system's `d3d9.dll`). The game no longer runs on the
user's Windows 11 machine; it should run natively, later with an uncapped frame rate (480 fps
target, logic unchanged), high resolution and widescreen. Milestone 1: the front end and the
missions playable with sound and music at the original frame rate.

Only code is ever shared; players supply their own game files. **Never commit anything
derived from the EXE** (translated code, extracted data); knowledge (addresses, names) is
fine. `game_files/`, `*.iso` and `compiled.bin` are git-ignored.

## The user

- New to reverse engineering; explain in plain words, give a recommendation, not a survey.
- Speaks German; works in short sessions and watches usage limits.
- Confirm before anything outward-facing or destructive (pushing, deleting).
- Prefers clean Rust over generated C; performance comes later.

## The game files

`game_files/` holds the installed game and `GunMetal.iso` (the CD image, git-ignored) lies in
the project folder. What matters:

- `Gunmetal.exe` (SHA-256 `aed26ffb...e6e4`, 32-bit x86 PE, base 0x400000, .text
  0x401000..0x5e3315): the whole game, C runtime and D3DX9 linked in. Only this build is
  supported: `Game::load` refuses any other with a message naming the expected hash.
- Data under `game_files/data`, `Manual/`, `Readme.htm`.

## How it works

The core is taken from rustschweine (2026-10-29) and made independent of Hogs of War; there is
only the EXE here, no engine DLL.

- `src/image.rs`: a PE file's sections (`exec`: holds code), imports, exports, resources.
  `data/symbols.txt`: what we know, by address.
- `src/game.rs`: loads `Gunmetal.exe` from the game folder after checking its SHA-256; seeds
  for finding the code: the entry point and every function of the Ghidra export.
- `src/ghidra.rs`: reads the Ghidra function export, `data/gunmetal_functions.json` (from the
  user, one object per line, 6904 functions), built in with `include_str!`.
- `src/discover.rs`: `find_functions` finds function boundaries: recursive descent from the
  seeds through calls; then code addresses found only as numbers (vtables, callbacks) when
  `plausible`. `Program::build` then decodes each function and adds entries where code jumps
  or calls into the middle of one. Result (2026-10-29): 9512 functions, 9793 entries (Ghidra
  has 6904: the rest are callbacks and vtable entries reached only through addresses).
- `src/lift.rs`, `src/lift/x87.rs`: x86 -> Cranelift IR (registers as variables, one block
  per instruction, tail calls between functions, `rt_lookup` for indirect targets, x87 with
  rounding modes). Not lifted yet: MMX, 3DNow! and SSE (D3DX9's alternative code paths,
  149 of 9793 entries are incomplete because of them; they compile with a stop where an
  instruction is missing, which ends the run with "unsupported instruction at ..: <it>").
  CPUID (`rt::rt_cpuid`) answers a Pentium Pro (GenuineIntel family 6: FPU, TSC, CX8, CMOV;
  no MMX/SSE/3DNow!, no extended leaves) so D3DX9 takes its x87 paths (it reads the bits at
  0x5b6d74, 0x5b3bdb, 0x5b3b2b); IsProcessorFeaturePresent says no to everything. EFLAGS AC
  and ID are kept by popfd/pushfd (`Cpu::ac`, `Cpu::id`); rdtsc is the host's TSC.
- `src/smc.rs`: fields of instructions the game writes (none found in the EXE).
- `src/rt.rs`: the CPU state, guest memory (4 GB reserved; guest address a = base + a),
  dispatch, events, x87 helpers. `GUNMETAL_COUNT=<file>`: functions count their calls.
- `src/jit.rs`: compiles all functions; the machine code can be cached (`compile_cached`,
  `compiled.bin` in the game folder, keyed by our own binary and the EXE).
- `src/iso.rs`: reading files from a CD image (.iso or raw .bin): its label, files by path,
  read in place (`IsoFile`).
- `src/main.rs`: `gunmetal [game folder]` (default `game_files`): finds, compiles (cached)
  and runs the game (`platform::run_game`).

### Platform layer (`src/platform/`, from rustschweine, 2026-10-29)

`mod.rs` (thunks for imports, guest heap, threads taking turns, calls into guest code, paths,
system DLLs for LoadLibraryA/GetProcAddress), `kernel32.rs`, `user32.rs` (the game's window
is guest bookkeeping, its window procedure guest code; GetSystemMetrics reports the size of
the display the window is on), `window.rs` (the real window with winit, no GL: `host_hwnd`
gives its HWND for Direct3D 9; it follows the game's SetWindowPos (`place`, also the size
input positions are scaled to) and WS_POPUP (`set_decorated`); Alt+Enter borderless
fullscreen, `display.rs` keeps the choice), `gdi32.rs` (a stock brush), `advapi32.rs` (registry in
`registry.txt`), `winmm.rs` (clock, timeSetEvent threads), `ole32.rs` (CoCreateInstance by
CLSID: `ole32::register_class(clsid, create)` for later modules; an unknown CLSID gives
REGDB_E_CLASSNOTREG and a log line), `dsound.rs` (DirectSoundCreate8 says no device),
`d3d9/` (Direct3D 9 passed through to the system's d3d9.dll, see below), `crt.rs` (`__ftol`
hooked; the game's warning report 0x557f40 goes to our log as "game: ..."),
`sample.rs` (`GUNMETAL_SAMPLE=1` profiler). MessageBoxA is the host's, over our window (scripted runs,
`GUNMETAL_INPUT`, answer it at once with its first button). Imports without an implementation stop the run
with "import X is not implemented yet" when called (listed at start: CreateProcessA,
EnumSystemLocalesA, file mappings, CreateFileW).

Direct3D 9 (`d3d9/`): every interface the game holds is a guest COM object (`com::wrap`) with
our vtable in the SDK's order (IDirect3D9 17 methods, IDirect3DDevice9 119); a method finds
the host interface behind `this` and calls the real one. Structures without pointers are
passed in place (guest memory is host memory at `mem().base`); D3DPRESENT_PARAMETERS and
D3DDEVICE_CREATION_PARAMETERS (an HWND) are converted (`types.rs`), D3DADAPTER_IDENTIFIER9 is
copied without the host's padding (1100 bytes). Methods without an implementation
(`method_not_implemented` in a method table) return 0 and are logged at their first call;
with `GUNMETAL_TRACE_API` COM methods also log their result ("-> 0x..."). CreateDevice adds
D3DCREATE_MULTITHREADED and, without D3DCREATE_FPU_PRESERVE, sets the calling thread's x87
control word to 24-bit precision as real D3D9 does (`fpu_after_create_device`).
16-bit display modes: the game wants every resolution in exactly two display formats
(d3d_prune 0x46afd0; XP drivers had R5G6B5 and X8R8G8B8); Windows 8 and later have no 16-bit
modes, so when the adapter has none, R5G6B5 modes are offered as the X8R8G8B8 ones and
R5G6B5 as a display format means X8R8G8B8 in the checks. Without that: "Nicht unterstützte
Grafikkarte.". `resources.rs`: textures, surfaces of textures, vertex and index buffers,
queries (what the game creates right after CreateDevice); locks give a guest buffer
(`com::shadow`) with the host's pitch, copied in unless D3DLOCK_DISCARD and out at the unlock
unless D3DLOCK_READONLY. Shaders: only "none" goes through. `shot.rs`: `GUNMETAL_SHOT`.

Paths: the game uses paths relative to its folder ("data\init.ini"); `host_path` makes a
guest path absolute against the current folder (at start `INSTALL_DIR`, C:\Spiele\Gun Metal,
also in GetModuleFileNameA), strips the install folder or the CD drive, and matches each part
case-insensitively in the game folder (`resolve_case`). D: is the CD drive: its paths mean the
game folder, and what the game folder lacks is read from the CD image (`GunMetal.iso` in the
game folder or above it, opened once: `cd_entry`, `cd_file`; CreateFileA and
GetFileAttributesA use it).

How the game starts (`GUNMETAL_TRACE_API=1` shows every import call):
1. C runtime start-up (heap, TLS, critical sections, environment, code pages), then WinMain
   (0x57a9b0) calls game_init (0x4579f0): CPU detection (0x5633d0: the EFLAGS ID toggle test,
   which pushfd/popfd keep, then CPUID and a 1 s busy measurement of rdtsc, the host's real
   TSC, against timeGetTime: cpu_mhz, e.g. 3403; above 1100 a first start picks the fast-CPU
   default of a detail setting at 0x459904), CoInitialize, then d3d_create (0x469530): LoadLibraryA("d3d9.dll") ->
   GetProcAddress("Direct3DCreate9") -> Direct3DCreate9(31), GetAdapterIdentifier(0).
2. The window (FindWindowA, RegisterClassA, CreateWindowExA "Gunmetal" 320x240, then
   data\init.ini, SetWindowLongA WS_POPUP, SetWindowPos to the desktop size from
   GetSystemMetrics, e.g. 2560x1440, ShowWindow); then game_start (0x457ae0): data\init.pck,
   the CD check, data\persist.dat (created if missing), d3d_enum_adapters (0x469760): per
   adapter GetAdapterModeCount for 5 formats, GetDeviceCaps, every mode with CheckDeviceFormat,
   CheckDeviceMultiSampleType, CheckDepthStencilMatch (resolutions 640x480..1600x1200).
   CreateDevice (adapter 0, HAL, flags 0x44 = hardware vertex processing | multithreaded):
   1024x768 fullscreen X8R8G8B8, 1 back buffer, 2x multisampling, discard, D24S8, immediate
   (the mode comes from data\persist.dat's defaults; the desktop size does not change it).
   Then render states, a vertex and an index buffer (dynamic, write-only), CreateQuery(event)
   as a support test, textures (DXT3 4x4 and 64x64 managed, A8R8G8B8 2048x2048 system memory,
   loaded through GetSurfaceLevel and LockRect), DirectSoundCreate8 (no device: the game goes
   on). **The run ends here now**: input_init (0x4ddcc0) asks CoCreateInstance for
   CLSID_DirectInput with IID_IDirectInput7W; without it the game logs "Joystick/joypad not
   found" (gunmetal.log in the game folder) and game_start fails: exit(1). Seen with input_init
   forced true (not kept): next the intro video's DirectShow filter graph (CLSID_FilterGraph,
   "Failed to create filter graph"), then a crash polling its IMediaEvent (0x586290). No
   Present comes before these.
3. Registry: the game reads none (its settings are data\init.ini and data\persist.dat); D3DX9
   asks HKLM\Software\Microsoft\Direct3D for debug switches (DisableMMX), which do not exist.
   `registry.txt` holds only our own settings (key HKLM\SOFTWARE\Rage\Gun Metal) and is written
   only when there are any.
4. CD check (0x459240 in game_start, skipped when d3d_create failed): every drive of
   GetLogicalDrives with GetDriveTypeA 5 and a working GetVolumeInformationA (the label is not
   compared) must have gmsetup\data2.cab with 128 bytes readable 1024 before its end and that
   offset above 0x3200000 (52 MB), else error string 16 of data\errormsg.txt and exit(1). Our
   answer: C: fixed, D: a CD-ROM labelled as the image ("NEW"), data2.cab read from the image
   (367 MB). Without an image the check fails unless the game folder has gmsetup\data2.cab.

Env: `GUNMETAL_TRACE_API` (imports with arguments), `GUNMETAL_TRACE_REG`, `GUNMETAL_TRACE_MSG`,
`GUNMETAL_INPUT` (scripted input, see `window.rs`), `GUNMETAL_WINDOW=WxH`,
`GUNMETAL_MONITOR=n`, `GUNMETAL_WATCH` (where the game is, every 2 s), `GUNMETAL_PROF`,
`GUNMETAL_SAMPLE`, `GUNMETAL_COUNT=<file>`, `GUNMETAL_SHOT=shot/x.png` (the back buffer
before Present, at most every `GUNMETAL_SHOT_MS`, 500; `{t}` in the name: ms since start;
`shot/` is git-ignored: game imagery is never committed).

Tools: `cargo run --release --example functions -- game_files [--gaps]` (find + compile,
statistics), `examples/dis.rs <folder> <addr|all>`, `examples/ghidra.rs <folder> [json]`
(discovery against the Ghidra export; `GUNMETAL_NO_GHIDRA=1` seeds with the entry point
alone), `examples/imports.rs <exe>`, `examples/resources.rs`. `cargo test --release` needs no
game files.
