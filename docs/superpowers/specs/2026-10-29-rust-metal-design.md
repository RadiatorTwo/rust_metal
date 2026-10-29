# rust_metal: Gun Metal recompiled to native code (design)

2026-10-29. Status: reviewed, waiting for approval.

## Intent

A native port of **Gun Metal** (Rage Games, PC, version 1.09.1.1, German) in Rust, built in
the same style as the user's two earlier ports, `C:\!Code2\future_rust_cop` (Future Cop:
L.A.P.D.) and `C:\!Code2\rustschweine` (Hogs of War): the game's own x86 code
(`Gunmetal.exe`) is decoded with iced-x86, lifted to Cranelift IR and compiled to native
code when the game starts; the Windows and DirectX functions it calls are implemented in
Rust.

Why: the game no longer runs on the user's Windows 11 machine at all (cause not known yet).
When it did, it had a hard frame-rate limit and only low resolutions; widescreen support is
unknown.

Milestone 1 (the user's words): get the game running with our code, with sound and music:
the front end and the missions are playable with sound effects and music, at the original
frame rate and resolution. "Playable" (my call): the first missions can be started and
finished, and saving and loading work; finishing every mission is not required for
milestone 1.

Milestone 2: uncapped frame rate (goal 3), then high resolution and widescreen (goal 4), and
the videos (goal 5); order inside milestone 2 is the builder's.

Releases (my call): like the siblings, builds for other Windows 11 PCs, so overrides in
`CreateDevice` / `Reset` and caps handling must not depend on the user's GPU.

## Goals

1. Gun Metal starts and is playable on Windows 11 x64 from the user's own game files.
2. Sound effects (DirectSound) and music (WMA through DirectShow) work.
3. Frame rate no longer capped: today the game is fixed at 30 fps; the user's target is
   480 fps, with the game's speed and logic unchanged.
4. High resolution and widescreen (Hor+ if the game does not already handle aspect).
5. Next after milestone 1: the WMV videos (intro, attract, cutscenes).

## Non-goals (for now)

- Linux. Windows only. Later possibly through DXVK-native (it provides D3D9 on Linux);
  nothing in the design should make that impossible, but nothing is done for it now.
- Translating D3D9 to OpenGL. Windows 11 runs D3D9 correctly; the game's D3D9 calls go to the
  real `d3d9.dll` (see below). This also means the game's shaders need no translation.
- Network play, mods, HD textures, gamepad remapping: not part of this design.
- Performance tuning beyond "runs well".

## Anti-goals

- **Higher frame rates must not change the game's physics or logic.** This is the user's
  explicit failure condition: "it must not run too fast". The original runs at a fixed
  30 fps; at 480 fps a frame-tied game would run 16 times as fast. A port where vehicles,
  projectiles, timers or AI move faster or behave differently than at 30 fps counts as
  broken. The test (my reading): the game at 480 fps plays at the same speed and behaves the
  same as at 30 fps; tick-for-tick identical results are not demanded, but where the logic
  steps per frame, keeping its 30 Hz step is the default because it guarantees this. If the logic
  is tied to the frame rate, the logic keeps its original step and only drawing gets faster
  (as future_rust_cop does in `src/platform/fps.rs`: logic at its original tick, pictures
  drawn in between, moving things interpolated). If that is not possible for some part, the
  frame rate stays capped rather than the physics changing.
- No game code or data in the repository or releases. Never commit anything derived from the
  EXE beyond knowledge (function addresses, names, descriptions): no translated code, no
  extracted data. `game_files/` and `*.iso` are git-ignored.
- No generated C; clean Rust, as in the sibling projects.

## Constraints and known facts

### The game files

`game_files/` (git-ignored) holds the installed game; `GunMetal.iso` (the CD image) sits next
to it in `C:\!Code2\rust_metal`.

- `Gunmetal.exe`: SHA-256 `aed26ffbf800da41c741e5d492a01fc1e49da2a54672c70dce48a93fe8cce6e4`,
  2,445,312 bytes, base 0x400000, not packed. Sections `.text` (0x1000, 0x1e2315),
  `.rdata` (0x1e4000), `.data` (0x21a000, virtual 0x157ffa), `.rsrc` (0x372000). Statically
  linked C runtime and MFC pieces, and D3DX9 (its shader assembler and compiler are inside
  the EXE: the game assembles vs_1_1 / ps_1_x shaders from text at run time).
- Static imports: KERNEL32 (114, including CreateThread, events, critical sections,
  GetDriveTypeA / GetVolumeInformationA: probably a CD check), USER32 (36), ole32
  (CoCreateInstance etc.), OLEAUT32, WINMM (timeSetEvent, timeGetTime, timeBeginPeriod),
  GDI32 (2), DSOUND (ordinal 11 = DirectSoundCreate8), ADVAPI32 (registry reads).
- Loaded at run time: `d3d9.dll` (`Direct3DCreate9`; also looks for `d3d9d.dll`).
- DirectShow through COM: `CLSID_FilterGraph`, `IGraphBuilder`, `IMediaControl`,
  `IMediaPosition`, `IMediaSeeking`, `IBasicAudio`, `IMediaEvent`; for videos the VMR9 in
  renderless mode with a surface allocator implemented by the game
  (`IVMRSurfaceAllocatorNotify9`, `VMRFilterConfig9`, a "texturerenderer" filter).
- Data: `data/*.pck` (init, mainfe, level, comsgerman; about 600 MB), `data/Init.ini`
  (language, pack-file switches), `data/Music/*.wma`, `data/music/frontend.wav`,
  `data/FrontEnd/*.wmv` (and `_h` variants), `data/SavedGames`, `data/SavedStates`.
- Whether input uses DirectInput 8 (through CoCreateInstance) or only GetAsyncKeyState and
  window messages is not established yet.

### The Ghidra export

`data/gunmetal_functions.json` (from the user, committed like the siblings' function lists):
6904 functions, each `{addr, size, name, thunk, chunks}`. 1192 have real names, nearly all C
runtime / MFC / D3DX (`__ftol`, `_strncpy`, `operator_new`, `CRect`, ...); game functions are
`FUN_xxxxxxxx`. Used as:
- extra seeds for function discovery,
- a cross-check: differences between our own discovery and Ghidra's are listed (an example,
  like rustschweine's `examples/ghidra.rs`),
- the initial content of `data/symbols.txt` (named functions only).

### `data/symbols.txt`

Same format as in rustschweine: address, name, description, `?` for guesses, grouped by
topic with `#` comments. Holds what we learn while debugging; committed (knowledge only).

## The how: decided parts

### Core (taken from rustschweine)

Copy rustschweine's core and make it independent of Hogs of War, as rustschweine did with
future_rust_cop: `image.rs` (PE file), `discover.rs` (function discovery without a Ghidra
export, now combined with the Ghidra seeds), `lift.rs` + `lift/x87.rs`, `smc.rs`, `rt.rs`
(CPU state, 4 GB reserved guest memory, guest address a = base + a, dispatch), `jit.rs` (with
the machine-code cache, `compiled.bin` next to the game), `iso.rs` (reading the CD image).
`game.rs` loads `Gunmetal.exe` and checks its SHA-256. Same Cargo setup (edition 2024,
iced-x86, cranelift 0.137, windows-sys), same `examples/` tools (dis, functions, ghidra,
imports, resources), same `CLAUDE.md` + `PLAN.md` hand-over style. Environment variables
follow the siblings' pattern with a `GUNMETAL_` prefix (e.g. `GUNMETAL_TRACE_API=1`).

SSE: the EXE was built in 2002/2003 with D3DX9, whose code paths may use SSE/SSE2 or 3DNow!
depending on CPUID. The lifter covers what the game executes; CPUID is answered so the game
takes paths the lifter supports (the builder decides which, after seeing the code).

### Platform layer

Win32 parts (kernel32, user32, gdi32, advapi32, winmm, ole32/oleaut32 as far as used) are
implemented in Rust as in the siblings, adapted to what Gun Metal calls. The window is a real
Win32 window (needed for D3D9).

Threads: the game calls CreateThread and uses events and timer callbacks (timeSetEvent).
How guest threads run is the builder's choice, informed by how the siblings handled it.

### D3D9: passed through to the real d3d9.dll

The game's D3D9 calls go to the system's `d3d9.dll`. The recompiled code is 32-bit inside
the guest memory window, the host process is 64-bit, so every D3D9 interface the game uses
(IDirect3D9, IDirect3DDevice9, textures, surfaces, vertex/index buffers, shaders, state
blocks, queries, ...) gets a thin wrapper:
- guest-side COM objects with vtables pointing at our functions, each holding the real host
  interface;
- pointer and structure translation (guest pointers are 32-bit; structures with pointer-sized
  fields such as the HWND in `D3DPRESENT_PARAMETERS` differ in layout);
- `Lock` returns a buffer in guest memory; `Unlock` copies it to the real resource (and back
  for read locks);
- shader bytecode from the game (assembled by its D3DX9) goes unchanged to
  `CreateVertexShader` / `CreatePixelShader`.

`CreateDevice` / `Reset` are where we set the resolution, windowed / fullscreen, VSync off,
and anti-aliasing; `GetAdapterModeCount` / `EnumAdapterModes` can offer modern resolutions
and widescreen modes to the game's own settings.

### DirectSound: passed through

DirectSound still exists on Windows 11. `DirectSoundCreate8` and the buffer interfaces are
wrapped the same way as D3D9 (guest vtables, `Lock` / `Unlock` through guest buffers).

### DirectShow: own small implementation over Media Foundation

The real DirectShow would call back into the game (VMR9 allocator, from its own threads), so
the interfaces the game uses are implemented in Rust instead, with Media Foundation
(built into Windows 11; WMA and WMV decoders included) doing the decoding:
- music: decode the WMA, play it ourselves (through the DirectSound pass-through or WASAPI,
  the builder's choice), with the game's volume (`IBasicAudio`), position and loop control;
- videos (after milestone 1): decode WMV frames and hand them to the game through its VMR9
  allocator path or directly into the D3D9 texture it draws; the builder decides after
  reading the game's video code. Until then, video requests finish at once (the game sees the
  video as ended).
- Windows N editions without the Media Feature Pack: no music or videos, a line in the log,
  the game still runs.

### Frame rate

First find how the game limits its rate (a timer, a Sleep, the presentation interval, a
fixed-step loop) and how the logic uses time. Then remove the limit only in a way that
leaves the logic's step unchanged (see Anti-goals). The original behaviour must stay
selectable for comparison (a setting, like future_rust_cop's `FrameRate = 0`).

There is no working original to compare against (the user has none at the moment). The
reference is the port itself at the original 30 fps: the same scene or replay at 30 and at
480 fps is compared (game speed, timers, positions of moving things over time). With D3D9
passed through, pictures in between logic steps mean running the game's own drawing code
again with interpolated positions (as future_rust_cop does); that reverse engineering is
expected.

### Files and start-up

The program is started from its folder and finds the game in `game_files/` (installed game
folder; reading files from the CD image through `iso.rs` is a later convenience, as in the
siblings). Settings / registry values the game reads go to a text file next to the game, as
in the siblings (`registry.txt`).

## Left to the builder

- Why the original no longer starts on Windows 11; relevant only if it points at something
  the port must handle (a CD check, a missing DLL, a driver check in the D3D9 caps
  enumeration).
- Whether the CD check needs an answer and what (drive type, volume name from the ISO).
- How threads, timer callbacks and COM objects implemented by the game are run.
- The exact list of D3D9 / DirectSound / DirectShow methods to wrap (from tracing what the
  game calls), the wrapper layout, and how resources and device loss / Reset are handled.
- Input: what the game uses, and whether gamepads need anything.
- How resolution and widescreen are offered (the game's own options vs. our settings), and
  what has to change for Hor+ if the game stretches.
- Order of work inside milestone 1, tests, and examples.
