# rust_metal

A native port of **Gun Metal** (Rage Games, 2002; German version 1.09.1.1) in Rust. The game's
own program is recompiled: the player's `Gunmetal.exe` is decoded with [iced-x86], every
function is lifted to [Cranelift] IR and compiled to native code when the game starts, and
the Windows/DirectX functions it calls are implemented in Rust (Direct3D 9 and DirectSound are
passed through to the system's `d3d9.dll` and `dsound.dll`, music is decoded with Media
Foundation). Built like [future_rust_cop] and [rustschweine]. No game code or data is part of
this repository: you need your own copy of the game.

Work in progress. Milestone 1 is built: the front end and the missions run with sound and
music at the original frame rate. See `PLAN.md` for what is done and what is open (videos,
gamepads and more).

## Playing

Windows only. You need the installed game (the folder with `Gunmetal.exe` and `data`) and the
CD image of the game (`GunMetal.iso`): the game's CD check wants a CD, and the port answers it
from the image.

1. Build: `cargo build --release`
2. Copy your installed game into a folder `game_files` and put `GunMetal.iso` into that folder
   or the one above it.
3. Run `target\release\gunmetal.exe [game folder]`; without a folder it uses `game_files`.
   The first start compiles the game and keeps the result in `compiled.bin` in the game folder,
   later starts are quick.

Only the PC version 1.09.1.1 is supported: the `Gunmetal.exe` with SHA-256 `aed26ffb...e6e4`.
With any other, the port says which one it expects. Messages go to `gunmetal.log`. Settings go
to `registry.txt`.

What is new compared with the original:
- A window, or borderless fullscreen (the default); Alt+Enter switches. The game's Direct3D
  device is never exclusive fullscreen, so Alt+Tab is quick.
- Any resolution of your display in the Video menu (the original stopped at 1600x1200), with
  widescreen (Hor+) in missions; the 4:3 menus stand in the middle.
- The HUD is laid out for a screen 768 pixels high and scaled up, so it is as big as at
  1024x768.
- Missions are not capped in frame rate. The game's logic still runs in 40 ms steps; the
  camera, model nodes, particles and trails are drawn between the last two steps, so pictures
  move smoothly. Set the DWORD `Smooth` to 0 in `registry.txt` (key `HKLM\SOFTWARE\Rage\Gun
  Metal`) to turn that off.
- Mouse: raw motion, wheel and five buttons; the cursor is caught while you play.

Not done yet: the videos (`.wmv`) are skipped, and there is no gamepad support.

## Environment variables

For finding problems; none is needed to play. All start with `GUNMETAL_`:
- `WINDOW=WxH`, `MONITOR=n`: the window's size, the display it opens on
- `TRACE_API`, `TRACE_REG`, `TRACE_MSG`: log the imports the game calls, its registry reads,
  its window messages
- `INPUT`: scripted keys and mouse at fixed times (the real keyboard and mouse are ignored)
- `SHOT=shot/x.png`: save the picture before each Present (`SHOT_MS`: at most every n ms)
- `STEPS`, `PROF`, `SAMPLE`, `COUNT=<file>`, `WATCH`: timing, profiling and where the game is
- `NO_MF=1`: no music

## Tools

```
cargo test --release
cargo run --release --example functions -- game_files
```

`cargo test --release` needs no game files. `CLAUDE.md` describes how everything works.

## Licence

MIT, see `LICENSE`. Gun Metal is the property of its owners; this project is not affiliated
with them.

[iced-x86]: https://github.com/icedland/iced
[Cranelift]: https://cranelift.dev
[future_rust_cop]: https://github.com/RadiatorTwo/future_rust_cop
[rustschweine]: https://github.com/RadiatorTwo/rustschweine
