# Plan — what is done, what is open

Updated 2026-10-31. Details on how things work: CLAUDE.md. Design:
`docs/superpowers/specs/2026-10-29-rust-metal-design.md`.

## Done

- [x] Recompiler core from rustschweine: image, discovery, x86 -> Cranelift, x87, SMC fields,
      runtime, JIT with cache, CD image reading; tests without game files
- [x] `Gunmetal.exe` loaded after a SHA-256 check; function discovery seeded with the entry point
      and the Ghidra export (`data/gunmetal_functions.json`, 6904 functions): 9506 functions, 9777
      entries (after the jump table fix below), all compile (149 entries with MMX/3DNow!/SSE parts
      as stubs)
- [x] Discovery against Ghidra without its seeds (`GUNMETAL_NO_GHIDRA=1`): of 6904 Ghidra
      functions 6886 are found. Fixed: a lone `fwait` made `plausible` reject real functions
      (3 found). The 18 left are unreferenced: C runtime helpers called through tables
      (`__cintrindisp1`, `__ctrandisp1/2`, `__trandisp1`, `__fload`, 0x58a399, 0x58ae33),
      0x5bfbc9 (a 3DNow! D3DX routine, 4436 bytes) and 0x5cbe0a (an exception funclet);
      nothing in the code or data points at them. With the Ghidra seeds none is missing.
- [x] Platform base from rustschweine (kernel32, user32, gdi32, advapi32, winmm, ole32 with
      `register_class`, window with winit and no GL): the game runs from the C runtime
      start-up on. Paths case-insensitive
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
      resolution). GetSystemMetrics is the real display (tests `tests/d3d9_types.rs`)

- [x] DirectInput 7 (`dinput.rs`, keyboard and mouse) and our own DirectShow filter graph and VMR9
      (`dshow.rs`): every video ends at once (EC_COMPLETE on Run, position at the end); the game
      passes its intro videos and presents its first frames. Both video players of the game
      (videotexture, which the game's settings choose, and VMR9, in an experiment with
      video_use_vmr9 forced true) were seen passing (tests `tests/dshow.rs`)

- [x] Scripted runs (`GUNMETAL_INPUT`) ignore the real keyboard, mouse and focus; the close
      button still counts (test `tests/input.rs`)

- [x] Front end drawn: cube textures, render targets and other device surfaces,
      vertex declarations, shaders (bytecode as it is), state blocks, every device method the
      front end calls, *UP draws in place; texture locks share one guest buffer per level;
      buffers keep their guest copy between locks (no read-back of write-only buffers);
      `xlat` lifted. The title screen and the main menu look like the manual's screenshots
      and the game idles in the menu (tests `tests/d3d9_lock.rs`, `tests/misc.rs`)

- [x] Missions start and run: 0x4d5a60 was missed because MSVC put a switch's
      jump table right after a call to `_exit`; decoding ran on into the table, whose bytes
      decode as `in`, so `plausible` rejected the function. Fix: a function's jump tables stop
      decoding (`Func::table_bytes`; test `table_after_noreturn_call`); 9506 functions, 9777
      entries (junk entries decoded from table bytes are gone). Seen: new game, slot, pilot ID,
      mission choice, arsenal, briefing, the first mission "Stunde X" with HUD, radar, enemies,
      the suit walking, turning and firing, explosions, damage; "Einsatz fehlgeschlagen" after
      ~95 s of scripted play (with a device Reset); a save created at the pilot ID and loaded
      after a restart.

- [x] Window mode and borderless fullscreen instead of exclusive fullscreen: the host's
      device is always windowed, the picture keeps its aspect ratio (black bars); Alt+Enter
      switches, saved in `registry.txt`, fullscreen by default (test `tests/picture.rs`).
      Seen: front end in both

- [x] DirectSound 8 passed through to the system's dsound.dll (`dsound.rs`):
      IDirectSound8, IDirectSoundBuffer8, IDirectSound3DBuffer, IDirectSound3DListener,
      IDirectSoundNotify; locks through a guest shadow of the whole buffer (same split as the
      host's); notification events forwarded from host events to the game's guest events
      (tests `tests/dsound_types.rs`). Seen: to the title screen every DirectSound call
      succeeds (two devices, primary 44100 Hz 16-bit stereo, the front end's samples loaded
      and played). Listening is the user's. GetLocalTime is local time now (save slot times
      were UTC; `tests/kernel32_time.rs`)

- [x] Device loss and Reset: the host device is windowed, so minimising and Alt+Tab
      do not lose it (seen). Focus and minimising now reach the game as WM_ACTIVATEAPP,
      WM_ACTIVATE, WM_SETFOCUS/KILLFOCUS and WM_SIZE (its pause on deactivation: video,
      music player, a mission's clock; asleep in GetMessageA while minimised). A loss (lock
      screen, UAC, mode change) makes the game release its default-pool resources and wait for
      a message; once the host's TestCooperativeLevel stops saying lost, its window gets a
      WM_PAINT and its next frame calls Reset. Seen with a simulated loss (`GUNMETAL_INPUT`
      `lose*2000`): Reset -> D3D_OK, the front end draws again; minimise/restore and blur/focus
      scripted (tests `tests/activation.rs`). To be confirmed by hand: Alt+Tab, Win+D, Win+L
      and back, in the front end and in a mission

- [x] Smooth pictures, first part (`fps.rs`, test `tests/fps.rs`): the camera (cam_set
      0x5491e0) and the model nodes (set_world 0x490610, ~470 a frame) are drawn between
      their places at the last two ticks (positions lerped, rotations slerped; one tick
      late), changed only for the drawing call and put back; the camera's globals are set
      again with the real values at the frame's end. Seen: the camera moves every frame
      (before: every 40 ms), the mission looks right, game time still follows the real
      clock; costs ~0.07 ms a frame. Registry `Smooth` = 0 turns it off

- [x] Smooth pictures, effects: particles (the game keeps each emitter's place at the last
      two ticks and blends them by a `t` it always passes as 0, 0x5123a0 -> 0x543b90; we pass
      how far between the ticks) and trails (0x53a2c0, point histories moved on at each tick:
      each point mixed between its last two ticks). Seen: trail points move every frame,
      particles look right. Measured (per caller of the sprite call 0x43d6d0): target markers
      are projected every frame with the drawn camera (onto the target's last tick place: off
      by what it moves in 40 ms, small)

- [x] Resolution and widescreen (`video.rs`): the Video menu offers every mode of the display
      (the game cut the list at 1600x1200 twice: the enumeration filter set at 0x459743, and
      the count 0x46dbc0); the menu switches at once and keeps the choice in persist.dat.
      Aspect: the monitor's shape (aspect_auto 0x549600) and the picture's 4:3 ("Normal",
      entry 0 of aspect_value 0x5499a0) are the back buffer's width / height, so missions
      fill any display undistorted and see more at the sides (Hor+) and the 4:3 menus stand
      in the middle (the game's own windowbox; the back buffer is cleared black after each
      Present). Seen at 2560x1440: menus, briefing, mission; 1024x768 unchanged

- [x] HUD at any resolution (`hud.rs`): the HUD's functions (0x43c210 each frame, the set-up
      0x43c890/0x43cf60, the sprite sheets' viewport 0x43cf30, the players' parts of the
      screen at 0x656980) see a screen 768 pixels high (1365x768 for 2560x1440), the overlay
      pages they draw are scaled up (0x48d8f0). Seen at 2560x1440: the panels, the crosshair
      and the target markers as at 1024x768; 1024x768 unchanged

- [x] Input (`dinput.rs`, `window.rs`): buffered keyboard data (GetDeviceData: taps
      between two reads), the mouse from raw motion (no stop at the edge), wheel and 5
      buttons, the cursor hidden and confined while the exclusive mouse is acquired and the
      window has the focus. To be confirmed by hand: menus with keys and mouse, quick taps,
      a changed key binding kept after a restart, aiming past the edge, Alt+Tab frees the
      cursor

- [x] Music: a graph given a sound file plays it (`dshow.rs`, `music.rs`, `mf.rs`:
      Media Foundation decodes the WMA, waveOut plays it, put_Volume scales it, EC_COMPLETE
      at the end, the game loops it); tests `tests/mf.rs`. To be confirmed by ear: front-end
      music, the menu's music volume, mission music

## Open for milestone 2

Milestone 1 (front end and missions playable with sound and music at the original frame rate)
is built; what still needs to be confirmed by hand and by ear is listed under Done
("to be confirmed").

- [ ] Frame rate: more than what is smoothed (see Done). Findings 2026-10-30
      (`src/platform/fps.rs`): missions are already uncapped. The game's own settings switch
      Game/variableUpdate on (maxFrameTime 40, variableUpdate 1, variableUpdateTime 40; not 30 as
      read earlier from the code's defaults), so game_frame (0x45e360) runs (real clock
      proj_data+0x360 - game time) / 40 logic ticks of 40 ms (0..3; behind by more, the clock is
      pulled to game time + 120, 0x4113f0) and draws every frame without a Sleep (~150 fps here).
      Game time follows the real clock, so the speed is right at any frame rate. But the logic moves
      in 40 ms steps (25 Hz) and the game does not interpolate: between two ticks every picture is
      the same (seen: the view matrix changes only at a tick). Per drawn frame only 0x44d9f0 gets
      the 40 ms step (`GUNMETAL_STEPS=1`; envelopes of a list at 0x661d6c whose results are not
      stored, probably Xbox rumble: harmless).
- [ ] Performance, 2026-10-31 (the user: 150 fps that swing are not acceptable; aim: steady
      several hundred fps). Order now: the recompiler's code first (it speeds the tick and the
      drawing alike), then measure again; the tick thread (design (a), below) is on hold.
      Why on hold: GetWriteWatch costs 0.65 ms a call (the plan assumed 0.1), the logic reads
      what the drawing writes between ticks, and the world copy needs three 4 GB worlds, a
      worker posing as the main thread and D3D replay, for a gain only in pictures with a tick.
      Benchmark: cold start to mission 1, then tick-indexed walking and turning (`t50:`..`t1300:
      close`), window 6-50 s after the mission's first picture; columns from `GUNMETAL_FRAMES`:
      guest time per picture without a tick (produce - imports) and logic per tick. Done:
      - flags cross calls, returns and jumps between functions only where read on the other
        side (`discover::FlagCrossings`: 1 call site of 44731 reads flags after the call, the
        C runtime's `__fload_withFB`; 21 entries read flags at entry);
      - stores of registers the function did not change are dropped (`lift::drop_silent_stores`;
        `Cpu` loads are `trusted`, so unused ones go too): a getter's code 121 -> 43 bytes;
      - `cmp`/`test` followed by jcc/setcc/cmovcc becomes one compare (`lift::Fused`).
      Result (2 runs each, before -> after): logic per tick 4.87 -> 4.05 ms, guest time per
      picture 4.74 -> 4.41 ms, fps 135 -> 143, pictures with a tick 11.6 -> 10.5 ms.
      Tried and dropped: x87 registers in Cranelift variables with the depth known when lifting
      (logic per tick 4.04 -> 4.43 ms: storing and reloading the eight slots at every call,
      `__ftol` included, costs more than the memory accesses it saves).
      Seen in the generated code (`GUNMETAL_DISASM=<addr>` with `examples/functions.rs`): each
      x87 arithmetic instruction is ~40 host instructions, most of it the 24-bit rounding check
      (Direct3D sets 24-bit precision); the code is 35 MB for 1.9 MB of x86. The profile is
      flat now: the largest guest function is 4.8 %, DrawIndexedPrimitive 5.9 %, Present 4.5 %.
      - Direct3D 9Ex with the flip model (`d3d9/ex.rs`, the user's choice to do first): 157 fps
        against 139-147 (one run), imports per picture 1.17 against 1.36-1.50 ms, Present 0.18
        against 0.23-0.32 ms on average; Present above 2 ms stays rare (6 of 8368). Seen: front
        end, mission, a simulated loss with Reset. To be confirmed by hand: Alt+Enter both
        ways, the Video menu's mode change (Reset), Alt+Tab, the picture's black bars.
      Tried and dropped too: the eight registers as arguments and results of guest functions
      (Cranelift's tail convention; logic per tick 3.80 -> 4.5 ms: three go on the stack, the
      results are shuffled and the caller spills around every call); an integer range check
      in the 24-bit rounding (no measurable change).
      Found on the way: a device loss in a mission (TestCooperativeLevel lost, not scripted)
      was followed by a null read in 0x47a6b0 (from 0x47a07a): to be looked at.
- [ ] Frame rate: even frame times (docs/superpowers/plans/2026-10-30-tick-decoupling.md,
      design (a) chosen: the tick on a second thread; on hold, see above). Baseline 2026-10-31, corrected
      (`GUNMETAL_FRAMES` without page probes, 3 cold runs of the profile script, mission 1,
      58-108 s with walking and turning; .superpowers/perf/task-spikes-report.md): 145-157 fps;
      intervals of pictures with a tick avg 11.5-12.0 ms, p99 20-21, max 30-32; without a tick
      avg 5.5-5.8, p99 10.3, max 15-17; all: avg 6.4-6.9, p99 16 (a third run was disturbed
      from outside: no-tick p99 14.9, and three 0.5-1.2 s stalls inside PeekMessageA's window
      pump). The first baseline (no-tick p99 17-20, max 28-30; tick p99 30-40) was mostly the
      probes' own cost: their finishing work (~15 ms after a probed frame with a tick) landed in
      the next, unflagged picture. What is left in pictures without a tick, all on the main
      thread, never waiting for the turn (no other guest thread runs then), no page faults:
      heavier scenes (twice the draws, ~8-10k imports a picture: CPU-bound, 10-17 ms) and the
      host's Present blocking 2-5 ms now and then (driver/DWM queue, ~55 % of the pictures
      that are 1.8x their neighbours). The tick is what remains. Findings for (a) (first
      baseline's runs, with probes):
      (1) the tick calls Direct3D: IDirect3DDevice9::SetLight ~12.5 and SetTransform ~1 per
      tick (no Create*, Lock, Draw or Present); DirectSound ~45 calls per tick (3D buffer
      SetPosition/SetVelocity ~14/~13, and about once per tick DuplicateSoundBuffer, Play,
      SetVolume, SetFrequency, Min/MaxDistance, sometimes Stop/Release); DirectInput
      GetDeviceData ~13 and GetDeviceState 3; HeapAlloc ~21, HeapFree ~19; critical sections
      ~300 and TlsGetValue/Get/SetLastError ~190 (the C runtime); no file I/O on the main
      thread (other threads: ~0.02 stream reads per tick).
      (2) 0x413c60 is 96 % of the tick (5.4-5.7 ms avg, max 24-34); game_clock_tick 0.15 ms;
      the rest under 0.05 ms.
      (3) pages written per tick ~825 (3.3 MB: heap ~705, COM lock buffers ~103, EXE .data
      ~13); the drawing after a tick ~1435 (heap ~735, lock buffers ~655, .data ~39), a whole
      picture with a tick ~1980 (7.9 MB), without a tick ~200-230 (0.8-0.9 MB).
      (4) pages written by both the tick and the drawing after it: ~282 (1.1 MB: heap ~218,
      lock buffers ~50, .data ~10), the same heap ranges in every probe. A world copy for (a)
      would have to sync ~3.3 MB of pages per tick, and the drawing writes ~1.1 MB of pages
      that the tick writes too (object/node state, not only renderer data)
      Step 0 of `docs/superpowers/plans/2026-10-31-tick-thread.md` (2026-10-31, `ticks.rs`,
      `GUNMETAL_TICK_PROBE`, `GUNMETAL_TICK_PRINT`, tick-indexed `GUNMETAL_INPUT` items `t<N>:`;
      report `.superpowers/perf/task-tt0-report.md`; mission 1, 651 ticks, ~14 cold runs):
      (1) C runtime heap type 1 (system heap): malloc/new go straight to HeapAlloc (~24 per
      tick, frees ~21), no small-block heap. Pools of the game's own: the router's id
      containers (slot arrays: units 512, type 4 1024, type 3 100) hand out slots.
      (2) GetWriteWatch costs ~0.65 ms per call whatever the range (4 KB or 150 MB; also in a
      reservation of its own, `examples/watchbench.rs`), ~2 ms over the whole 4 GB; 5 committed
      ranges 4 ms. The plan's 0.1 ms per call is out of reach: step 1 needs one call per fork at
      most, or a dirty map of our own (stores in the generated code marking pages). Committed
      guest memory in a mission: 160 MB (3x = 480 MB, fits).
      (3) The ~100 "COM lock buffer" pages per tick are false sharing: small texture-level,
      index and vertex buffer shadows (16 B..128 KB) share their pages with game heap blocks; no
      shadow page is written whole by a tick.
      (4) World update 0x413c60 per picture with a tick avg 5.8 ms (p50 5.6, p99 13.6, max 20.6)
      against the drawing after it 7.0 (p50 7.1, p99 11.5); longer than the drawing in 13-18 %
      of the tick pictures (25 % with page probes): step 6 matters for those.
      (5) Vtable +0x4c of world+0x2c (the router) after the tick loop is 0x504360: per frame it
      writes the router's +0x90a0 (game time) and +0x90a4 (game time - last tick's, x 0.03:
      always 0) and walks its list of ~1590 objects calling method +0x44, which is 0x40d0a0
      (`ret 4`) for all of them: no logic, but the list's cursor (llist pCurrent, +0xc) is
      written. Flag 0x665193: set by a debug key in game_frame's typed-key switch, cleared after
      the loop, read nowhere else.
      (6) `_rand`: ~162 calls per tick (particles/effects 0x53c..0x544, 0x4d0f78), none outside
      ticks in a mission (only in the front end); srand(0) at the mission's start.
      (7) Determinism: two runs with the same tick-indexed input agree in game time, camera
      place and object counts for all 651 ticks; `_rand` order and some unit values differ in
      a few ticks from run to run, and a forced 2-3-tick frame (`stall*130`) changes the order
      of an emitter's `_rand` calls in the second tick of that frame (0x53d933.., from late to
      early in the tick), later the number of live units: the logic reads state the drawing
      writes between ticks (an effects list's order, unit values). Smooth (fps.rs) on against
      off changes +0x150..+0x158 of 22 objects of class 0x5e6d88 from tick 3 on (deterministic
      in each setting): the logic reads something the smoothing changes. Seen once in ~15 runs:
      a null read in 0x504360's list walk (llist pCurrent) while the mission loaded.
- [ ] Smooth pictures, left at the tick (small): water (0x530da0, its waves if they move),
      the radar's dots (0x444035, 0x4440e6), a trail's texture scroll (+0x98); look at it
      in play (does anything still shake?)
- [ ] Frame rate, later: a frame-rate limit setting; performance for 480 fps (now ~5.4 ms a
      frame in a mission); the game's demo recorder (Demo/recordDemo, playDemo) to compare
      the same play at two rates. Profile and ranked fixes:
      `.superpowers/perf/2026-10-30-mission-profile.md` (local, git-ignored). Done (P1, 2026-10-30): indirect
      dispatch through a direct table (`rt::Direct`) instead of a binary search: rt_lookup
      14.2 % -> 1.5 % of the main thread, the profile's mission script ~154 -> ~185 fps
      (6.5 -> 5.4 ms a picture). Next: an inline target check at indirect call sites (or the
      table read inline in generated code, skipping the host call), then fix 2 (flags around
      calls) and 3 (static x87 TOP)
- [x] R6025 "pure virtual function call" (2026-10-31, fixed in `router.rs`): not a thread
      race and not our emulation but a latent bug of the game, hit when it is closed
      (WM_CLOSE: Alt+F4 on the original, our window's close button, a script's `close`) while a
      mission is starting (after the briefing's "BEREIT", before the mission music; closing
      at 47 s in the cold-start script crashes every time without the fix). WinMain's loop
      ends, game_shutdown 0x45c430 deletes the world (0x413cb0) and its router's destructor
      (0x503140) deletes the eleven id containers in tree post-order; type-2 objects (unit
      dtor 0x41db70) then remove a type-5 id they hold ([+0x1f8], also 0x5ffffff) through the
      router, whose type-5 container was deleted first (vtable already 0x5e45fc): _purecall.
      In a running mission the router's lists (0x503440) remove those objects earlier.
      The fix: while a router is being destroyed, removing an id whose container it already
      deleted is skipped (returns "not found"; logged as "game: id ... skipped"). The 1-in-10
      catches came from scripts closing at 62 s when the mission start ran late. Also:
      `GUNMETAL_TRACE_LIFE=1` logs the main loop's step returning false, WM_CLOSE/WM_DESTROY,
      the loader thread's state changes and game_shutdown ("game: ..."); `GUNMETAL_TRACE_ROUTER=1`
      logs the router's handler deletions. Report: `.superpowers/perf/task-r6025-report.md`
- [ ] Resolution, still open: the door transition is stretched to a wider back buffer; the
      menu background's scrolling text shows faintly beside the 4:3 middle; the radar is
      wider than round (it follows the screen's width); our own resolution setting (the
      game's needs its menu, which takes keys only in parts)
- [ ] Joysticks/gamepads: the game enumerates DIDEVTYPE_JOYSTICK (joy_enum 0x4dd6f0) and
      goes on without; needs a joystick device (DIJOYSTATE, objects, ranges; gilrs)
- [ ] Videos (`.wmv`): the graph still skips them (`dshow.rs`)
- [ ] Direct3D 9 still open: volume textures, swap chains, GetDC, GetAdapterMonitor (none
      called by the front end)
- [ ] Imports bound to "not implemented" (stop when called): CreateProcessA,
      EnumSystemLocalesA, CreateFileW, CreateFileMappingA, MapViewOfFile, UnmapViewOfFile
- [ ] MMX/3DNow!/SSE in the lifter: not needed while CPUID hides them
- [ ] Names for `data/symbols.txt` (Ghidra has 1192 non-`FUN_` names: C runtime, MFC, D3DX)
