# Plan — what is done, what is open

Updated 2026-10-30. Details on how things work: CLAUDE.md. Design:
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

- [x] DirectInput 7 (`dinput.rs`, keyboard and mouse) and our own DirectShow filter graph and
      VMR9 (`dshow.rs`): every video ends at once (EC_COMPLETE on Run, position at the end);
      the game passes its intro videos and presents its first frames (black: drawing is not
      implemented yet). Both video players of the game (videotexture, which the game's
      settings choose, and VMR9, in an experiment with video_use_vmr9 forced true) were seen
      passing (tests `tests/dshow.rs`)

- [x] Scripted runs (`GUNMETAL_INPUT`) ignore the real keyboard and mouse; the close button
      and focus changes still count (test `tests/input.rs`)

- [x] Front end drawn (Task 6): cube textures, render targets and other device surfaces,
      vertex declarations, shaders (bytecode as it is), state blocks, every device method the
      front end calls, *UP draws in place; texture locks share one guest buffer per level;
      buffers keep their guest copy between locks (no read-back of write-only buffers);
      `xlat` lifted. The title screen and the main menu look like the manual's screenshots
      and the game idles in the menu (tests `tests/d3d9_lock.rs`, `tests/misc.rs`)

- [x] Missions start and run (Task 11): 0x4d5a60 was missed because MSVC put a switch's
      jump table right after a call to `_exit`; decoding ran on into the table, whose bytes
      decode as `in`, so `plausible` rejected the function. Fix: a function's jump tables stop
      decoding (`Func::table_bytes`; test `table_after_noreturn_call`); 9506 functions, 9777
      entries (junk entries decoded from table bytes are gone). Seen: new game, slot, pilot ID,
      mission choice, arsenal, briefing, the first mission "Stunde X" with HUD, radar, enemies,
      the suit walking, turning and firing, explosions, damage; "Einsatz fehlgeschlagen" after
      ~95 s of scripted play (with a device Reset); a save created at the pilot ID and loaded
      after a restart. No sound (DirectSound fails; the game goes on)

- [x] Window mode and borderless fullscreen instead of exclusive fullscreen: the host's
      device is always windowed, the picture keeps its aspect ratio (black bars); Alt+Enter
      switches, saved in `registry.txt`, fullscreen by default (test `tests/picture.rs`).
      Seen: front end in both

## Open

- [ ] Frame rate (milestone 2). Findings 2026-10-30 (`src/platform/fps.rs`): missions are
      already uncapped. The game's own settings switch Game/variableUpdate on (maxFrameTime
      40, variableUpdate 1, variableUpdateTime 40; not 30 as read earlier from the code's
      defaults), so game_frame (0x45e360) runs (real clock proj_data+0x360 - game time) / 40
      logic ticks of 40 ms (0..3; behind by more, the clock is pulled to game time + 120,
      0x4113f0) and draws every frame without a Sleep (~150 fps here). Game time follows the
      real clock, so the speed is right at any frame rate. But the logic moves in 40 ms steps
      (25 Hz) and the game does not interpolate: between two ticks every picture is the same
      (seen: the view matrix changes only at a tick). Per drawn frame only 0x44d9f0 gets the
      40 ms step (`GUNMETAL_STEPS=1`; envelopes of a list at 0x661d6c whose results are not
      stored, probably Xbox rumble: harmless).
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
- [ ] Smooth pictures, left at the tick (small): water (0x530da0, its waves if they move),
      the radar's dots (0x444035, 0x4440e6), a trail's texture scroll (+0x98); look at it
      in play (does anything still shake?)
- [ ] Frame rate, later: a frame-rate limit setting; performance for 480 fps (now ~6-8 ms a
      frame in a mission); the game's demo recorder (Demo/recordDemo, playDemo) to compare
      the same play at two rates
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
- [ ] Resolution, still open: the door transition is stretched to a wider back buffer; the
      menu background's scrolling text shows faintly beside the 4:3 middle; the radar is
      wider than round (it follows the screen's width); our own resolution setting (the
      game's needs its menu, which takes keys only in parts)
- [ ] DirectInput: buffered keyboard data (GetDeviceData; the game uses it for short presses),
      joysticks (gamepads), the exclusive mouse's relative movement at the window's edge
- [ ] Videos (`.wmv`) and music: the graph skips them now (`dshow.rs`)
- [ ] Direct3D 9 still open: volume textures, swap chains, GetDC, GetAdapterMonitor (none
      called by the front end)
- [ ] DirectSound (`dsound.rs`: DirectSoundCreate8 says "no device" now)
- [ ] Imports bound to "not implemented" (stop when called): CreateProcessA,
      EnumSystemLocalesA, CreateFileW, CreateFileMappingA, MapViewOfFile, UnmapViewOfFile
- [ ] MMX/3DNow!/SSE in the lifter: not needed while CPUID hides them
- [ ] Names for `data/symbols.txt` (Ghidra has 1192 non-`FUN_` names: C runtime, MFC, D3DX)
