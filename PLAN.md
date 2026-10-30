# Plan — what is done, what is open

Updated 2026-10-30. Details on how things work: CLAUDE.md. Design:
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
- [ ] R6025 "pure virtual function call" at mission load (rare: 1 in 10 cold starts to
      mission 1, 2026-10-30, caught by `crash.rs`; earlier 1 in 9). It is the main thread,
      not a guest thread: game_frame's teardown 0x45c2b0 -> 0x534860 deletes the front end's
      world (0x413cb0, proj+0x6a8); its object tree (0x503140/0x5095a0) deletes a 512-slot
      container (dtor 0x45bf40), whose items (dtor 0x41db70) unregister their id ([+0x1f8])
      from the router (proj vtable +0x14 -> +0x2c, method +0x44 = 0x507890). The router finds
      the handler for the id's top byte in its tree at +0x9084 and calls its +0xc: that
      handler (ecx 0x2343c3f0) already has the abstract base vtable 0x5e45fc, i.e. it was
      destroyed and is still registered. Meanwhile the loader thread 0x4eadf0 had finished
      (0x4e7120 done, waiting out its 1 s in Sleep(100)); stream threads were in waits. Likely
      cause: the loader thread's objects and the front end's share the router, and how their
      registrations interleave (turns at imports and loop heads) decides whether a handler
      is unregistered before the items using it. Next: log the router's registrations and
      removals per thread (and the handler's destruction) in the failing order, then make
      that part of loading not interleave (or remove the stale entry when a handler dies).
      Needed before design (a) of docs/superpowers/plans/2026-10-30-tick-decoupling.md
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
