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
  `data/symbols.txt`: what we know, by address (empty so far).
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
  149 of 9793 entries are incomplete because of them; they compile with a stub where an
  instruction is missing).
- `src/smc.rs`: fields of instructions the game writes (none found in the EXE).
- `src/rt.rs`: the CPU state, guest memory (4 GB reserved; guest address a = base + a),
  dispatch, events, x87 helpers. `GUNMETAL_COUNT=<file>`: functions count their calls.
- `src/jit.rs`: compiles all functions; the machine code can be cached (`compile_cached`,
  `compiled.bin` in the game folder, keyed by our own binary and the EXE).
- `src/iso.rs`: reading files from a CD image (.iso or raw .bin).
- `src/platform.rs`: stand-in until the platform layer exists (an empty `HOOKS`, `cstr`).
- `src/main.rs`: `gunmetal [game folder]` (default `game_files`): finds and compiles the code,
  then stops ("platform layer not implemented yet").

Tools: `cargo run --release --example functions -- game_files [--gaps]` (find + compile,
statistics), `examples/dis.rs <folder> <addr|all>`, `examples/ghidra.rs <folder> [json]`
(discovery against the Ghidra export; `GUNMETAL_NO_GHIDRA=1` seeds with the entry point
alone), `examples/imports.rs <exe>`, `examples/resources.rs`. `cargo test --release` needs no
game files.
