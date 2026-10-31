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
- `src/discover.rs`: `find_functions` finds function boundaries: recursive descent from the seeds
  through calls; then code addresses found only as numbers (vtables, callbacks) when `plausible`.
  `Program::build` then decodes each function and adds entries where code jumps or calls into the
  middle of one. A function's jump tables stop decoding (MSVC puts them after calls that do not
  return, e.g. `_exit`). Result (2026-10-30): 9506 functions, 9777 entries (Ghidra has 6904: the
  rest are callbacks and vtable entries reached only through addresses).
- `src/lift.rs`, `src/lift/x87.rs`: x86 -> Cranelift IR (registers as variables, one block
  per instruction, tail calls between functions, `rt_lookup` for indirect targets, x87 with
  rounding modes; xlat, which the C runtime's __trandisp2 uses). Not lifted yet: MMX, 3DNow!
  and SSE (D3DX9's alternative code paths,
  about 150 entries are incomplete because of them; they compile with a stop where an
  instruction is missing, which ends the run with "unsupported instruction at ..: <it>").
  CPUID (`rt::rt_cpuid`) answers a Pentium Pro (GenuineIntel family 6: FPU, TSC, CX8, CMOV;
  no MMX/SSE/3DNow!, no extended leaves) so D3DX9 takes its x87 paths (it reads the bits at
  0x5b6d74, 0x5b3bdb, 0x5b3b2b); IsProcessorFeaturePresent says no to everything. EFLAGS AC
  and ID are kept by popfd/pushfd (`Cpu::ac`, `Cpu::id`); rdtsc is the host's TSC.
- `src/smc.rs`: fields of instructions the game writes (none found in the EXE).
- `src/rt.rs`: the CPU state, guest memory (4 GB reserved; guest address a = base + a),
  dispatch, events, x87 helpers. `GUNMETAL_COUNT=<file>`: functions count their calls.
  Dispatch: `rt_lookup` (every indirect call or jump, ~6 million a second in a mission) reads
  `Direct`, a table with one host code pointer per guest address from the lowest entry to the
  highest (~1.9 million slots, 15 MB, built in `compile_cached` for cached code too): an
  entry's code or the foreign stub (then `Cpu::foreign` = the address). `Dispatch::table`
  (sorted pairs) stays for the sampler and crash reports.
- `src/jit.rs`: compiles all functions; the machine code can be cached (`compile_cached`,
  `compiled.bin` in the game folder, keyed by our own binary and the EXE).
- `src/iso.rs`: reading files from a CD image (.iso or raw .bin): its label, files by path,
  read in place (`IsoFile`).
- `src/main.rs`: `gunmetal [game folder]` (default `game_files`): finds, compiles (cached)
  and runs the game (`platform::run_game`).

### Platform layer (`src/platform/`, from rustschweine, 2026-10-29)

`mod.rs` (thunks for imports, guest heap, threads taking turns, calls into guest code, paths, system
DLLs for LoadLibraryA/GetProcAddress), `kernel32.rs`, `user32.rs` (the game's window is guest
bookkeeping, its window procedure guest code; GetSystemMetrics reports the size of the display the
window is on), `window.rs` (the real window with winit, no GL: `host_hwnd` gives its HWND for
Direct3D 9; it does not follow the game's SetWindowPos or WS_POPUP (`place` only sets the size input
positions are scaled to): it is a framed window that a device's back buffer size fits
(`set_game_size`, centred), or borderless fullscreen; Alt+Enter switches, `display.rs` keeps the
choice, fullscreen by default; ShowCursor hides the cursor over it; an acquired exclusive
DirectInput mouse hides and confines it while the window has the focus, `catch_mouse`, not in
scripted runs; raw mouse motion is `Input::Motion`; focus changes and minimising reach the game's
window as WM_ACTIVATEAPP, WM_ACTIVATE, WM_SETFOCUS/WM_KILLFOCUS and WM_SIZE, `user32::activation`:
the game pauses while inactive and sleeps while minimised), `gdi32.rs` (a stock brush),
`advapi32.rs` (registry in `registry.txt`), `winmm.rs` (clock, timeSetEvent threads), `ole32.rs`
(CoCreateInstance by CLSID: built-in DirectInput, filter graph and VMR9, and
`ole32::register_class(clsid, create)` for later modules; an unknown CLSID gives REGDB_E_CLASSNOTREG
and a log line), `dinput.rs` (DirectInput 7 from rustschweine: keyboard by state and buffered events
(GetDeviceData, DIPROP_BUFFERSIZE, DIGDD_PEEK, DI_BUFFEROVERFLOW; 16-byte DIDEVICEOBJECTDATA), mouse
by state from raw motion (`user32::take_motion`, wheel 120 per notch, 5 buttons); devices track
Acquire (GetDeviceState/GetDeviceData unacquired: DIERR_NOTACQUIRED, the game re-Acquires); no
joysticks: the game enumerates them and goes on without), `dshow.rs` (our own DirectShow filter
graph and VMR9: videos are skipped, Run posts EC_COMPLETE and the position is the end, 0; a sound
file (RenderFile: data\music\*.wma, by the game's music object, music_* in symbols.txt) gets a
player, `music.rs`: a host thread decodes it with `mf.rs` (Media Foundation's IMFSourceReader to
16-bit PCM; mfplat.dll and mfreadwrite.dll loaded at run time, not imported, so the exe starts on
Windows N without them; MFStartup once; `mf::available` asks for a WMA decoder, `GUNMETAL_NO_MF=1`
says no: "no Media Foundation WMA decoder: music off" and the music is silent, ended at once) and
plays it through waveOut, put_Volume scaling the samples (a file or device that does not open: no
player, silent); at the end of the file EC_COMPLETE through the graph's event (set by
`kernel32::set_event_state` from the host thread), and the game loops by SetPositions(0)),
`dsound.rs` (DirectSound 8 passed through to the system's dsound.dll as `d3d9/` is: IDirectSound8,
IDirectSoundBuffer8, IDirectSound3DBuffer/Listener, IDirectSoundNotify as guest COM objects;
DSBUFFERDESC's format pointer points at the game's WAVEFORMATEX in place; Lock gives parts in a
guest shadow of the whole buffer, first part at the lock's offset, a wrapped second at its start,
the host's bytes copied in, the game's out at Unlock; SetNotificationPositions maps each guest event
to a host event whose thread sets the guest one, `host_event`), `d3d9/` (Direct3D 9 passed through
to the system's d3d9.dll, see below), `crt.rs` (`__ftol` hooked; the game's warning report 0x557f40
goes to our log as "game: ..."), `sample.rs` (`GUNMETAL_SAMPLE=1` profiler; `=54000` starts it after 54 s), `video.rs` (the Video
menu offers every display mode, not only up to 1600x1200; the monitor and the game's 4:3 picture are
the back buffer's own shape: Hor+ widescreen in missions, the 4:3 menus in the middle), `crash.rs`
(the C runtime's fatal errors: `__amsg_exit` 0x588d97 hooked; R6025 and the others log the thread,
`ecx` and its vtable, the ebp chain and a stack scan for return addresses, and every guest thread's
last import with its stack, then exit as the original; `GUNMETAL_TRACE_LIFE=1` adds hooks that log
the main loop's step returning false, WM_CLOSE/WM_DESTROY, the loader thread's state changes and
game_shutdown as "game: ..."; the hook set is part of compiled.bin's key),
`router.rs` (the world's id router; fixes the game's own R6025 when it is closed while a mission
starts: the router's destructor deletes its containers in tree order and later objects remove
ids through containers already deleted; such a removal is skipped. `GUNMETAL_TRACE_ROUTER=1`
logs the deletions), `frames.rs` (`GUNMETAL_FRAMES=<file>`: a
line per picture, 2 s summaries, findings for the tick on a second thread at the exit), `hud.rs`
(the HUD laid out for a screen 768 pixels high and scaled up: as big as at 1024x768), `fps.rs` (the
frame rate: missions are uncapped with 40 ms logic ticks; the camera, model nodes, particles,
trails, the zoom (the projection's fov), target markers and the crosshair's aim point are drawn
interpolated (the crosshair's per-frame half step made one of time) between the last two ticks (by the host's clock when
the frame draws), registry `Smooth` = 0 turns it off;
`GUNMETAL_STEPS=1` logs where the step is asked outside ticks). MessageBoxA is the host's, over our
window (scripted runs, `GUNMETAL_INPUT`, answer it at once with its first button). Imports without
an implementation stop the run with "import X is not implemented yet" when called (listed at start:
CreateProcessA, EnumSystemLocalesA, file mappings, CreateFileW).

Direct3D 9 (`d3d9/`): with `GUNMETAL_D3D9EX=1` a 9Ex device with the flip model (`ex.rs`; off by default:
RivaTuner's overlay vanished with it; plain D3D9 as described below otherwise): the game's back buffer is our render
target (its size, format and MSAA, with our depth-stencil surface; GetBackBuffer hands it out), the
swap chain has the game's size, 2 buffers, FLIPEX, frame latency 1; Present resolves and
StretchRects the picture into it with the aspect kept (black around it) and calls PresentEx.
MANAGED does not exist on 9Ex: managed buffers are created in the default pool, managed textures
and cube textures in the default pool with a system-memory twin that their locks (also through a
level's surface) go to, uploaded with UpdateTexture at each unlock; GetDesc/GetLevelDesc still say
MANAGED. No device loss on 9Ex (TestCooperativeLevel S_OK; the simulated loss still works).
Every interface the game holds is a guest COM object (`com::wrap`) with our
vtable in the SDK's order (IDirect3D9 17 methods, IDirect3DDevice9 119); a method finds the host
interface behind `this` and calls the real one. Structures without pointers are passed in place
(guest memory is host memory at `mem().base`); D3DPRESENT_PARAMETERS and
D3DDEVICE_CREATION_PARAMETERS (an HWND) are converted (`types.rs`), D3DADAPTER_IDENTIFIER9 is copied
without the host's padding (1100 bytes). Methods without an implementation (`method_not_implemented`
in a method table) return 0 and are logged at their first call; with `GUNMETAL_TRACE_API` COM
methods also log their result ("-> 0x..."). CreateDevice adds D3DCREATE_MULTITHREADED and, when it
succeeds without D3DCREATE_FPU_PRESERVE, sets the calling thread's x87 control word as real D3D9 and
Wine do: 24-bit precision, rounding to nearest, exceptions masked (`fpu_after_create_device`). Never
exclusive fullscreen: the host device is always windowed (`with_present_params`; the game sees its
own fullscreen parameters back, the display mode stays the desktop's), and Present puts the back
buffer into our window with its aspect ratio, black bars painted with GDI (`letterbox`,
`window::picture`), and then clears the back buffer black (`clear_back_buffer`: the game draws only
the 4:3 middle of a wider one in its menus). A fullscreen device of the game's makes input positions
its back buffer's pixels (`window::set_game_size`) until a Reset to windowed. 16-bit display modes:
the game wants every resolution in exactly two display formats (d3d_prune 0x46afd0; XP drivers had
R5G6B5 and X8R8G8B8); Windows 8 and later have no 16-bit modes, so when the adapter has none, R5G6B5
modes are offered as the X8R8G8B8 ones and R5G6B5 as a display format means X8R8G8B8 in the checks
and as a fullscreen back buffer in CreateDevice/Reset (`host_back_buffer_format`; the game sees its
own format back). Without that: "Nicht unterstützte Grafikkarte.". `resources.rs`: textures, cube
textures, surfaces, vertex and index buffers, queries, vertex declarations (the D3DVERTEXELEMENT9
array copied to its end marker), vertex and pixel shaders (the game's bytecode as it is, assembled
by its own D3DX9), state blocks. Locks give a guest buffer (`com::shadow`) with the host's pitch,
one per object and level (a cube face's level its own): a buffer's covers the whole buffer, a
texture level's the whole level, and a lock's range or rect is at its offset, so nested locks share
it; a part of a surface of unknown format is refused. Copied in unless D3DLOCK_DISCARD
(`lock_copy_in`), out at the unlock unless D3DLOCK_READONLY. Buffers keep their guest copy between
locks: only bytes it never held are read from the host (the game's dynamic buffers are write-only in
the default pool and locked with NOOVERWRITE hundreds of times a second; reading write-combined
memory back would be slow), ProcessVertices makes it stale (`resources::stale`). Textures and
surfaces are read at every lock. Every device method the game calls goes through; *UP draws hand
D3D9 the game's arrays in place after a check that they are committed guest memory. Not implemented:
swap chains, volume textures, patches, GetNPatchMode, GetDC. `shot.rs`: `GUNMETAL_SHOT`. Device loss
(`device.rs` module docs): a windowed device is not lost by minimising or Alt+Tab (seen), but can be
by the lock screen, UAC, Ctrl+Alt+Del or a mode change. The game then releases its default-pool
resources and waits in GetMessageA; `device::lost_ended` repaints its window (WM_PAINT) once the
host's device is no longer lost, so its next frame calls Reset (logged "d3d9: Reset -> 0x0"). Seen
with the simulated loss (`lose*ms`): Reset succeeds and the front end draws again.

Paths: the game uses paths relative to its folder ("data\init.ini"); `host_path` makes a
guest path absolute against the current folder (at start `INSTALL_DIR`, C:\Spiele\Gun Metal,
also in GetModuleFileNameA), strips the install folder or the CD drive, and matches each part
case-insensitively in the game folder (`resolve_case`). D: is the CD drive: its paths mean the
game folder, and what the game folder lacks is read from the CD image (`GunMetal.iso` in the
game folder or above it, opened once: `cd_entry`, `cd_file`; CreateFileA and
GetFileAttributesA use it).

How the game starts (`GUNMETAL_TRACE_API=1` shows every import call):
1. C runtime start-up (heap, TLS, critical sections, environment, code pages), then WinMain
   (0x57a9b0) calls game_init (0x4579f0): CPU detection (0x5633d0: the EFLAGS ID toggle test, which
   pushfd/popfd keep, then CPUID and a 1 s busy measurement of rdtsc, the host's real TSC, against
   timeGetTime: cpu_mhz, e.g. 3403; above 1100 a first start picks the fast-CPU default of a detail
   setting at 0x459904), CoInitialize, then d3d_create (0x469530): LoadLibraryA("d3d9.dll") ->
   GetProcAddress("Direct3DCreate9") -> Direct3DCreate9(31), GetAdapterIdentifier(0).
2. The window (FindWindowA, RegisterClassA, CreateWindowExA "Gunmetal" 320x240, then data\init.ini,
   SetWindowLongA WS_POPUP, SetWindowPos to the desktop size from GetSystemMetrics, e.g. 2560x1440,
   ShowWindow); then game_start (0x457ae0): data\init.pck, the CD check, data\persist.dat (created
   if missing), d3d_enum_adapters (0x469760): per adapter GetAdapterModeCount for 5 formats,
   GetDeviceCaps, every mode with CheckDeviceFormat, CheckDeviceMultiSampleType,
   CheckDepthStencilMatch (resolutions 640x480..1600x1200). CreateDevice (adapter 0, HAL, flags 0x44
   = hardware vertex processing | multithreaded): 1024x768 fullscreen X8R8G8B8, 1 back buffer, 2x
   multisampling, discard, D24S8, immediate (the mode comes from data\persist.dat's defaults; the
   desktop size does not change it). Then render states, a vertex and an index buffer (dynamic,
   write-only), CreateQuery(event) as a support test, textures (DXT3 4x4 and 64x64 managed, A8R8G8B8
   2048x2048 system memory, loaded through GetSurfaceLevel and LockRect), DirectSound (sound_init
   0x465130, twice: two devices; primary 44100 Hz 16-bit stereo, a looping 1 s buffer, the samples).
   input_init (0x4ddcc0): DirectInput 7 three times through CoCreateInstance (CLSID_DirectInput,
   IID_IDirectInput7W, then QueryInterface IID_IDirectInput7A, Initialize 0x700; di_create 0x4dded0)
   for the keyboard, the mouse and the joysticks (none: "Joystick/joypad not found" in gunmetal.log,
   which is fine). How the game reads them (`dinput.rs` module doc): keyboard non-exclusive, by
   state (kb_read 0x4dced0) and, for a key not down, the buffered presses since the last read
   (kb_key_down 0x4dcdc0, kb_buffered 0x4dcce0); mouse exclusive | foreground by state (mouse_read
   0x4dd0d0; game_start unacquires it, the first read re-acquires it), driving the game's own menu
   cursor and the aim (mouse_update 0x4477d0); key bindings (bind_capture 0x4ed4f0) take keys, mouse
   buttons 0-6 and the wheel; the bindings file is data\KeyAssignments.txt (missing: defaults). The
   game reads no window messages for input. Then the videos (video_start 0x4174a0; `dshow.rs` has
   the details): Extras/VideoMethod is 1, so the videotexture player (filter graph, AddSourceFilter,
   the game's own "TEXTURERENDERER" filter, IMediaEvent) plays "cgplayed_01_h.wmv",
   "intro1_ps_h.wmv", "intro2_ps_h.wmv", each ending at once (one Present each, white: the game
   draws the video quad with DrawPrimitive). Then the front end: Clear (depth and stencil only),
   DrawIndexedPrimitive, Present, its textures. The front end: a white frame per
   intro video (the video quad without a video), then the door-opening transition, the title screen
   (frame with the rotating 3D "GUN METAL" logo, version "V1.09.1.1" top right) at ~3.5 s, and after
   a key the main menu: "Neues Spiel", "Spiel laden", "Spiel löschen", "Gun Metal beenden" on the
   turning globe, inside the same frame as the manual's screenshots
   (`game_files/Manual/Files/Images/Screens`). After ~40 s on the title the attract video
   (`attract_ps_h.wmv`, skipped: white) plays and the title returns. To the main menu:
   `GUNMETAL_INPUT="16000:Enter" GUNMETAL_SHOT=shot/menu.png` (16 s leaves the title time to load;
   while `GUNMETAL_INPUT` is set the real keyboard and mouse are ignored, `window::reaches_game`:
   only the close button counts; focus and minimising come from the script: `blur`, `focus`,
   `minimize`, `restore`; `lose*2000` simulates a device loss). To the first mission from a cold
   start (no saves; it creates save slot 0, delete it afterwards): `GUNMETAL_INPUT="16000:Enter
   19000:Up 20000:Enter 24000:Enter 28000:Enter 32000:Enter 36000:Enter 40000:Enter 44000:Enter
   48000:Enter 50000:move@512,384"` (title, Up to "Neues Spiel", slot, pilot ID OK, "Einsatz
   wählen", mission, arsenal "Starten", briefing "BEREIT"; the mission runs from ~52 s). In scripted
   runs DirectInput's mouse movement is the script's moves (the first one a starting point); in real
   runs it is raw mouse motion. In the mission W walks, a held left button fires, mouse movement
   aims. The menu's drawing: fixed function only (SetVertexShader(null)/SetPixelShader(null)); FVFs
   0x102 (XYZ|TEX1), 0x142 (XYZ|DIFFUSE|TEX1), 0x112 (XYZ|NORMAL|TEX1), 0x42 (XYZ|DIFFUSE), 0x202,
   0x152; DrawPrimitive triangle strips of 2 (quads, from a dynamic vertex buffer, 0x478650) and
   DrawIndexedPrimitive triangle lists (dynamic buffers 0x478760, meshes 0x4727f0); per frame
   Clear(Z and stencil only, 0xff000080), BeginScene, EndScene, Present. Textures: DXT1, DXT3 and
   A8R8G8B8, managed; cube textures DXT1 (4x4 and 64x64 with 7 levels, managed). Dynamic buffers:
   two vertex buffers (FVF 0x202 and 0x144) and one 16-bit index buffer, usage DYNAMIC|WRITEONLY in
   the default pool, locked NOOVERWRITE and DISCARD when full (0x4781c0, 0x4783b0); static buffers
   managed. At start-up the game makes one vertex shader with a declaration (0x4790b0) and one pixel
   shader (0x479890), assembled by its D3DX9 (which asks d3d9.dll for
   Direct3DShaderValidatorCreate9: not there, it goes on), sets the vertex shader once (0x47c3f0)
   and then draws without shaders.
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
`GUNMETAL_INPUT` (scripted input, see `window.rs`; real keyboard and mouse ignored; items
`t<N>:<what>` are delivered at the start of mission tick N, durations in ticks; `@file.txt` reads
them from a file, `#` comments), `GUNMETAL_RECORD=file.txt` (`record.rs`: the player's input as
such items, ms outside missions and mission ticks in them: keys and mouse buttons with their
hold, raw motion summed per tick or 50 ms as `motion@dx,dy`, `wheel@n`, focus, close; F11 writes a
mark comment and `file.txt.<ms>.png`),
`GUNMETAL_WINDOW=WxH`, `GUNMETAL_MONITOR=n`, `GUNMETAL_WATCH` (where the game is, every 2 s),
`GUNMETAL_PROF`, `GUNMETAL_SAMPLE`, `GUNMETAL_STEPS`, `GUNMETAL_TRACE_ROUTER`, `GUNMETAL_TRACE_LIFE`, `GUNMETAL_NO_MF` (music off),
`GUNMETAL_COUNT=<file>`, `GUNMETAL_FRAMES=<file>` (per picture: Present time, interval, time to
produce it, ticks and their time, and where the main thread's time went: imports, the slowest one,
waits for the turn and who held it, page faults, CPU time; summaries every 2 s and at ExitProcess;
at the exit the imports called inside ticks and time per tick part; with `GUNMETAL_FRAMES_PAGES=1`
also guest pages written by the ticks and the drawing, from write-watch probes every 20th picture,
which the summaries leave out with the picture after each, and the COM lock buffers' pages by
owner; at the exit also the world update 0x413c60 against the drawing after it, per picture),
`GUNMETAL_TICK_PRINT=<file>` (`ticks.rs`: a logic fingerprint per mission tick, values only:
game time, camera place, objects per id type, hashes of the units and the player's object,
`_rand` calls and seed; compare two runs with `cargo run --release --example tickprint -- a b`;
`GUNMETAL_TICK_PRINT_UNITS=<tick>[,..]` adds the units' hashed values at those ticks,
`GUNMETAL_TICK_PRINT_RAND=1` each tick's `_rand` callers in order; a script item `stall*ms` sleeps
the main thread so the next frame runs 2-3 ticks),
`GUNMETAL_TICK_PROBE=1` (the tick thread's step-0 measurements, logged at the exit: C runtime heap
type, malloc/free and `_rand` in and outside ticks with callers, the router's per-frame method,
GetWriteWatch costs and the committed size; either TICK variable adds `ticks::HOOKS`, part of the
cache key), `GUNMETAL_SHOT=shot/x.png` (the back buffer before Present, at most every
`GUNMETAL_SHOT_MS`, 500; `{t}` in the name: ms since start; `shot/` is git-ignored: game imagery is
never committed).

Tools: `cargo run --release --example functions -- game_files [--gaps]` (find + compile,
statistics), `examples/dis.rs <folder> <addr|all>`, `examples/ghidra.rs <folder> [json]`
(discovery against the Ghidra export; `GUNMETAL_NO_GHIDRA=1` seeds with the entry point
alone), `examples/imports.rs <exe>`, `examples/resources.rs`, `examples/tickprint.rs a b`
(first difference of two fingerprint files), `examples/watchbench.rs` (GetWriteWatch's cost
without the game), `examples/flags.rs <folder>` (where flags cross calls and returns).
`GUNMETAL_DISASM=<hex addr>[,..]` prints those functions' Cranelift IR and machine code when they
are compiled (with `examples/functions.rs`, which does not use the cache). `cargo test --release` needs no
game files.
