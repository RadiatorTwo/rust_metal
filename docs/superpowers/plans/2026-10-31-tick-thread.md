# Even frame times in missions: the world update on a second thread

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or
> superpowers:executing-plans. Steps use checkbox (`- [ ]`) syntax.

**Goal:** in missions, a picture with a logic tick takes about as long as one without. The
tick's cost leaves the main thread's critical path and runs on a second host thread while the
main thread draws. No latency is added. Each tick computes exactly what the original computes
from the same state and the same input.

**Decision (user, 2026-10-30): design (a)** of `2026-10-30-tick-decoupling.md`. Pacing (d) was
rejected because of its latency. Facts used here: PLAN.md (frame rate),
`.superpowers/perf/task-p0-p1-report.md`, `task-spikes-report.md` (git-ignored notes).

## 1. The design

### 1.1 What moves where

The tick loop in game_frame (0x45ed8b..0x45eea3) calls parts 0-5 (game_clock_tick, 0x457640
with kb_read, 0x4a5620, 0x4c49d0, 0x449430, 0x44c300) and then `0x413c60(proj+0x2c)`
(thiscall, `ret 4`; the loop ignores eax and does `dec esi`). 0x413c60 is 96 % of the tick
(5.4-5.7 ms on average, p99 12-15 ms, maximum 24-34 ms). Parts 0-5 take together about 0.2 ms.

- **Parts 0-5 stay on the main thread, in the real memory (world A), as today.** Game time, the
  frame step `+0x4cc`, real_clock_update and the 0-3 tick count therefore stay right in A every
  frame. Nothing re-counts ticks while one is still running.
- **0x413c60 ("P6") runs on a worker host thread, in a copy (world B).** The P6 hook on the main
  thread *forks*: it brings B up to date, gives the worker a copy of the main thread's Cpu
  (registers, x87 state and control word, fs) with `mem` = B's base, and returns at once as
  `ret 4` would (`esp += 8`). The main thread then goes on with the rest of game_frame: the
  per-frame code after the loop and the drawing.
- **The worker runs as the main guest thread.** Same thread id, same TLS values, same last
  error, and B's copy of the main thread's stack at the same esp. So it needs no stack of its
  own, and the C runtime's per-thread data (`_getptd`) is the main thread's, in B.
- **Join:** the merge of B into A happens at a frame boundary (1.4). From the next picture on,
  the drawing shows the new tick.

### 1.2 Why the drawing gets the real memory and the tick the copy

Whichever side runs in the copy has its side effects handled specially. Per tick the tick makes
about 60 such calls (DirectSound ~45, DirectInput ~16, SetLight ~12, SetTransform ~1). Per
picture the drawing makes about 1000 Direct3D calls, plus dynamic-buffer locks, managed
resources, listener commits, HUD and menu side effects, and the window. Putting the tick in the
copy means special handling for those ~60 calls only.

Rejected:
- **Tick in the real memory on the worker, drawing in a copy:** the drawing's lasting writes
  (renderer and its D3D state caches, dynamic-buffer positions, HUD, menus) would all have to be
  merged back: the same merge problem, with every D3D object on the copy's side.
- **(a2) A compact snapshot of what the drawing reads:** the drawing walks the object tree, ~470
  nodes, particle systems, HUD, effect manager; a "snapshot" of that is the world itself.
- **Moving the whole tick (parts 0-6):** game time in A would be stale while the tick runs; the
  next frames would count ticks again, set the frame step and pull the clock back (clock_set).
  That would need patching game_frame.

### 1.3 Memory model

- **Three reservations of 4 GB each:** A (the real one, `MEM_WRITE_WATCH`), B (the tick's,
  `MEM_WRITE_WATCH`) and C (the base: A as it was at the fork). Every `Memory::map` (heap growth,
  image, stacks, TIB) is mirrored into all three while the feature is on: committed memory in a
  mission about 3x A's (to be measured).
- **Write-watch ranges:** only the mapped ranges (image .data, TIB and stacks, the heap arena up
  to its mapped top), never the whole 4 GB (a probe over 4 GB cost ~4 ms).
- **Initial copy:** at the first eligible mission tick, A's mapped ranges are copied in full
  into B and C; A's write watch is reset first, so writes during the copy are caught later.
- **Background sync (worker, between ticks; it is idle ~34 of every 40 ms):** take A's written
  pages with a reset, add the pending list, copy those pages A->B and A->C. A page the main
  thread writes during the copy is flagged again and copied in the next pass.
- **Fork (main thread):** copy the leftover written pages of A (about one picture's worth, ~200
  pages, ~1.6 MB, ~0.15 ms) to B and C, reset B's write watch, wake the worker.
- **Join and merge (main thread at a frame boundary, worker finished):**
  - WB = B's written pages, WA = A's written pages since the fork.
  - A page in WB but not in WA: copy B->A.
  - A page in both (~280 measured): copy only the bytes where B differs from C (the tick wins),
    except structures with their own rule (1.6).
  - The worker builds the byte masks (B against C) before it signals that it is done.
  - The main thread's stack below the fork's esp and its TIB's SEH word are not merged.
  - WA and WB go onto the background pending list.
  - Expected main-thread cost per tick: fork <= 0.2 ms plus merge <= 0.5 ms.
- **Why C exists:** without the state at the fork, nobody can tell which bytes on a page written
  by both the tick changed; copying a whole page from B would wipe what the drawing wrote on it.
  C can later become sparse (only pages the drawing ever writes).
- **Known blind spot:** a byte the tick writes with the value C already had keeps the drawing's
  value. Only possible on bytes both sides write; check mode lists candidates.

### 1.4 Threads, turns and when to join

- **Turns:** world A keeps today's turn lock (main thread, stream_thread, music workers, loader).
  The tick thread never takes A's turn: `on_event` skips `turn_give`/`turn_take` for it;
  `rt_yield` returns at once for it (one thread-local check); `EXCLUSIVE` applies to A only.
- **Thread role:** thread-local `ROLE` (Main, Tick, Other). `mem()` reads the thread-local
  world: A by default, B on the tick thread.
- **Join points:**
  1. game_frame's entry (hook): join if the worker is done. Step 4 also blocks here; step 6
     lets the tick run on past it.
  2. Any tick-part hook (0-5) or P6 while a tick is still running: blocking join first (2-3
     ticks in one frame: the second tick's parts run only after the first's P6 is merged).
  3. The main thread calling an import outside game_frame while a tick runs: blocking join.
     The message pump, mission teardown and saving always see a merged world.
- **fps.rs at join:** the camera re-set at frame end (`guest_call(CAM_SET)`) runs after the
  merge.

### 1.5 Imports called from the tick thread

| Kind | Rule on the tick thread |
|---|---|
| HeapAlloc, VirtualAlloc, GlobalAlloc, realloc | One shared host allocator (unique addresses for both worlds). A new block is zero-filled in A, B and C. |
| HeapFree, VirtualFree, GlobalFree, COM Release at 0 | While a tick runs, frees from any thread are queued and done after the merge. |
| Enter/Leave/Init/DeleteCriticalSection | A table per world; B's starts as a copy of A's entries owned by the main thread. |
| TlsGet/SetValue, Get/SetLastError, thread id | The main thread's values; TlsSetValue recorded and done at join; last error to the main thread at join. |
| Interlocked* | Memory of the caller's world (`mem()`). |
| timeGetTime, QueryPerformanceCounter, GetTickCount | Live: the tick runs at the same wall time as today. |
| DirectInput (GetDeviceState, GetDeviceData) | Live; host state behind `DEVICES`. The tick is the only reader of input in a mission. |
| DirectSound (SetPosition, SetVelocity, Duplicate, Play, Stop, ...) | Live, in the tick's order. Guest COM objects created in B and merged; Release at 0 queued like a free. |
| D3D setters (SetLight, LightEnable, SetTransform, SetRenderState, SetMaterial) | Recorded with arguments copied by value, D3D_OK returned; replayed on the main thread at join, in order, after the merge. |
| Any other D3D, file, registry, window or wait call | Not expected (measured). Check mode: a loud log line. Normal mode: called live plus a warning once. |

### 1.6 Merge rules (the logic stays the same)

- **Meaning of the merged state:** the drawing of the pictures that ran during the tick, then the
  tick. The tick starts from exactly A after parts 0-5, with the same input read at the same
  moment and the same FPU control word, so it computes what the original computes, as long as it
  does not read anything the drawing wrote during the overlap.
- **Idempotent drawing writes are fine:** e.g. node world places +0x40 have the same values in
  every picture of the same tick.
- **Default rule:** the tick's bytes win.
- **Rules per structure, found in step 4:** drawing wins (per-picture renderer data the tick
  touches only by false sharing); add up (A + B - C: reference counts, counters); drawing writer
  queued (drawing-side changes to pools or lists the tick also changes, done after the merge);
  serial (no safe rule: the tick runs on the main thread whenever that structure is in use).
- **Expected entries:** `_rand`'s seed at ptd+0x14 (tick wins: like a picture that drew no random
  number; the original's sequence already depends on the frame count); flag 0x665193 cleared by
  game_frame after the loop; camera globals 0x72a7c8/0x72a7d8; the light list 0x661d30; particle
  pools.

### 1.7 Pictures between ticks (fps.rs)

- A's game time already includes the running tick (part 0 ran in A), but A's objects still show
  the tick before. fps.rs gets `world_t`, the game time of the newest merged state (fork: game
  time - step; join: game time). `alpha = (real clock - world_t) / step`; `place()` labels its
  tracks with `world_t` instead of reading +0x4dc.
- **While a tick runs (step 4):** alpha clamped to 1; the one picture drawn during the tick shows
  the last tick.
- **Step 6 (a tick over several pictures):** alpha up to 1 + cap/step; positions extrapolated
  linearly, rotations by slerp with t > 1; particles clamped to 1. Nothing shifted in time.

### 1.8 Latency

Today the tick's result is on screen at fork + P6 + drawing. With (a): fork + max(P6, drawing) +
merge + drawing. The same as today when P6 <= drawing (5.5 against 5.5 ms now). When P6 is
slower, at most about one picture later than today's slow tick picture would have been.

### 1.9 When the tick runs in parallel (otherwise on the main thread as today)

All must hold: the feature is on; `alpha()`'s conditions (mission, variableUpdate, clock moving);
at least K = 60 steady mission frames; the loader thread 0x4eadf0 has ended; no guest thread
other than the main thread holds a critical section; the device is not lost (0x76d51d is 0) and
no Reset is in progress; the initial copy is done. Pausing, Alt+Tab, minimising, in-mission menus,
saving, loading screens and teardown are covered by these or by join point 3. Device loss while
a tick runs: join as normal; replaying setters on a lost device does no harm.

### 1.10 Switch and modes

Registry `TickThread` (DWORD in the game's key; 0 = off, the default until step 7) and
`GUNMETAL_TICK_THREAD`: `0` / `1` off / on; `serial` (fork, the main thread waits at once: tests
the machinery without overlap); `check` (overlap plus the reports of 1.11).

### 1.11 Finding divergence

- **Check mode reports:** every byte both sides changed (A != C and B != C), grouped by heap
  block (the allocator keeps each block's allocation return address in check mode) or by .data
  symbol; pages written by both without a rule; tick writes to the main thread's stack at or
  above the fork's esp+8; tick imports outside the table above.
- **Logic fingerprint per tick** (`GUNMETAL_TICK_PRINT=<file>`): game time, camera place, the
  player vehicle's place and state, number of live objects. Values only, no addresses.
- **Tick-indexed input:** a `GUNMETAL_INPUT` variant that delivers events at tick numbers.
- **The test:** `off` against `on` give identical fingerprints, if two `off` runs already agree
  (measured in step 0).

## 2. Components

- `src/rt.rs`: `Memory` gets a write-watch flag; mapped ranges kept (`Ranges`).
- `src/platform/world.rs` (new): worlds A, B, C and the thread-local current world;
  `written(world, ranges, reset)`, `copy_pages`; `masks` and `merge` (pure functions on byte
  slices); background sync and pending list; the merge rules table.
- `src/platform/tick.rs` (new): the worker thread, its Cpu, fork and join; eligibility; D3D
  recorder and replayer; queued frees; frame-log columns.
- `src/platform/mod.rs`: `mem()` per thread, the heap shared by all worlds, `ROLE`, turns skipped
  for the tick thread, join point 3 in `on_event`.
- `src/platform/kernel32.rs`: a critical-section table per world; TLS and thread-id
  impersonation.
- `src/platform/d3d9/device.rs`: setters recorded on the tick thread.
- `src/platform/fps.rs`: the P6 hook forks; join points 1 and 2; `world_t`, alpha clamp and cap;
  `IN_TICK` per thread.
- `src/platform/frames.rs`: columns fork_ms, join_wait_ms, merge_ms, pages_synced, pages_merged,
  conflict_bytes, inflight_pictures.
- `src/platform/crash.rs`: the tick thread registered as "tick (main, world B)"; `report_fault`
  knows all three bases.

## 3. Steps

### Step 0: measurements that decide details (no change in behaviour)

- [x] (all step-0 items done 2026-10-31, answers in section 5) At mission start, log the C runtime's heap type at 0x770EA8 (1 = system heap: malloc is
      HeapAlloc one to one; 3 = small-block heap: then hook malloc 0x5888b4, free, realloc,
      _msize onto the host allocator in step 2). Find any of the game's own pools
      (`GUNMETAL_COUNT` on malloc against HeapAlloc per tick).
- [ ] Cost of GetWriteWatch with reset over the mapped ranges, and A's committed size in a
      mission. Goal: <= 0.1 ms per call; 3x the commit fits.
- [ ] Which object and key the ~103 "COM lock buffer" pages per tick belong to.
- [ ] P6 alone against drawing time: distribution, per picture.
- [ ] What game+0x2c vtable +0x4c (after the loop) and flag 0x665193 do: who sets, who reads.
- [ ] `_rand` calls inside and outside ticks.
- [ ] Tick-indexed input and the fingerprint (1.11). Two `off` runs: identical fingerprints? If
      not, find the reason first; then the equivalence test uses only what is deterministic.
- [ ] Tests: the fingerprint format; the tick-indexed script parser.
- [ ] Findings in PLAN.md. Commit.

### Step 1: three worlds (off by default)

- [ ] `world.rs` with A, B, C; `mem()` reads the thread-local world; `map` mirrored; initial
      copy; background sync; merge with masks and rules.
- [ ] Tests (small reservations of their own): a page written on one side is copied; byte merge
      on a page both wrote, the tick winning; the add-up rule; a write during a copy is flagged
      again; excluded ranges left alone.
- [ ] Measure with the feature off: fps within +-1 % of the baseline (cost of a per-thread
      `mem()`). Commit.

### Step 2: thread roles and imports per role (still off)

- [ ] `ROLE`; the worker posing as the main thread (thread id, TLS, last error); critical
      sections per world; shared allocator with zero-fill in all worlds and queued frees; D3D
      recorder and replayer.
- [ ] Turns skipped and `rt_yield` a no-op for the tick thread; `IN_TICK` per thread; crash
      registration.
- [ ] Tests: queued frees not handed out again before the join; critical sections in world B do
      not block world A; a recorded SetLight replays the same bytes; TLS impersonation.
- [ ] Commit.

### Step 3: `serial` mode

- [ ] The P6 hook forks; the main thread waits at once; join, replay, `world_t`; eligibility.
- [ ] Runs: 3 scripted mission runs including `lose*2000`, blur/focus and minimise; an
      in-mission menu and leaving the mission by hand; 10 cold starts to the mission (back up
      and restore the saves).
- [ ] Expected: the same play; fingerprints equal to `off` (if deterministic); fork <= 0.2 ms,
      merge <= 0.1 ms; no new crash.
- [ ] Commit.

### Step 4: overlap and check mode (joined at the latest at the next game_frame)

- [ ] The main thread draws while the worker runs P6; join points 1-3; alpha clamped to 1.
- [ ] `check`: 3 runs of at least 2 minutes; a list of conflicting addresses grouped by
      structure, saved with the findings.
- [ ] Measure (`GUNMETAL_FRAMES`, 3 runs): fork + merge on the main thread <= 0.7 ms per tick;
      with-tick intervals about max(drawing, P6) + 0.7.
- [ ] Commit, still off by default.

### Step 5: a rule for every conflict

- [ ] One rule per structure from step 4, each with a sentence explaining why.
- [ ] `check` until only explained conflicts remain.
- [ ] A 10-minute mission by hand (the user): fights, explosions, deaths, saving, Alt+Tab.
- [ ] Fingerprints equal to `off` with tick-indexed input.
- [ ] Commit.

### Step 6: a tick running over several pictures

- [ ] At game_frame's entry, join only if the worker is done (or wait at most W, default 1 ms);
      otherwise draw again with alpha extrapolated (cap 0.25). Join points 2 and 3 still block.
- [ ] Tests (`tests/fps.rs`): alpha and `world_t` across fork and join; the cap; extrapolation of
      position and slerp.
- [ ] Measure (3 runs, window 58-108 s as in the baseline): with-tick avg <= 1.15x without-tick
      avg (today 2.1x); with-tick p99 <= without-tick p99 + 1.5 ms (today 20 against 10);
      overall p99 <= 1.3x avg as far as pictures without a tick allow (alone 1.8x today); fps >=
      baseline + 10 %; pictures per tick with a tick still running logged.
- [ ] By eye (the user): turning, aiming, explosions; no snapping at joins.
- [ ] Commit.

### Step 7: on by default and hand-over

- [ ] `TickThread` defaults to 1 if steps 5-6 passed.
- [ ] CLAUDE.md (worlds, roles, the switch, check mode, fingerprints) and PLAN.md (results,
      remaining p99 causes).
- [ ] Commit.

## 4. Risks and what to do about them

- **Structures both sides change** (pools, lists, reference counts, random seed, HUD and menu
  state): check mode makes them visible; each gets a rule; `serial` is the fallback per
  structure.
- **The C runtime's small-block heap or the game's own pools** would hand out the same address
  in both worlds: step 0 measures; then malloc is hooked onto the host allocator and pools get
  rules.
- **Write watch missing writes** (kernel writes such as ReadFile into guest buffers): check mode
  compares A with C plus merged pages over the whole range every Nth join; our ReadFile copies
  through a host buffer if needed.
- **Cost of GetWriteWatch and copying:** ranges restricted to mapped memory; the worker does the
  background sync; C sparse later.
- **Direct3D calls from the tick other than the setters:** recorded or logged.
- **Input and sound ordering:** input read only in the tick, at the same moment as today; sound
  calls in the tick's own order; a listener commit by the drawing can catch half of a tick's
  deferred 3D settings for one picture (~5 ms, not audible).
- **FPU state:** the Cpu copy carries fcw; the worker copies the main thread's MXCSR at start;
  fingerprints would show a difference.
- **Memory:** about 3x the committed guest memory in a mission; measured in step 0.
- **R6025:** (a) adds no guest thread to world A and keeps A's turn lock, but depends on the fix:
  the first fork waits until loader thread 0x4eadf0 has ended; mission teardown (proj teardown
  0x534860, world destructor 0x413cb0, the crash's path) must never run while B holds an
  unmerged tick (join point 3); a fix through `EXCLUSIVE` or a hook of 0x45c2b0 fits (world A
  only). Steps 3-6 are measured only once the fix is in.

## 5. Open questions (measure first, step 0)

Answered in step 0 (2026-10-31; `.superpowers/perf/task-tt0-report.md`, PLAN.md):

1. The C runtime heap type (0x770EA8) and the game's own pools. **1, the system heap: malloc is
   HeapAlloc one to one** (~24 per tick). No hook of malloc needed. Own pools: the router's id
   containers (slot arrays); slots taken in a tick need the tick-wins rule.
2. GetWriteWatch cost over the mapped ranges; committed size of A. **~0.65 ms per call, fixed
   (any range size); 2 ms over 4 GB.** 0.1 ms is not reachable with write watch: one call per
   fork over one range at most, or our own dirty-page map written by the generated stores.
   **A: 160 MB committed** (3x fits).
3. What the ~103 lock-buffer pages written per tick are. **False sharing**: small shadows
   (texture levels, index/vertex buffers) share their pages with heap blocks the tick writes.
   No rule needed for shadows; the merge must work on bytes there (B against C).
4. vtable +0x4c after the loop and 0x665193: per-frame drawing code, or logic that expects the
   tick's result? **0x504360: per frame, writes router +0x90a0/+0x90a4 and its list cursor;
   calls `ret 4` for every object: harmless (tick wins). 0x665193: a debug key's flag, read
   nowhere.**
5. Are fingerprints of two `off` runs with tick-indexed input identical (determinism)? **No,
   not entirely**: game time, camera and object counts are; the `_rand` order and some unit
   values depend on how ticks fall into frames (the drawing between two ticks changes what the
   next tick reads, e.g. an effects list's order), and Smooth on/off changes unit values. The
   equivalence test needs a fixed frame schedule (one tick per frame, or the same frames:
   a lockstep test clock) or compares only time/camera/objects.
6. P6 against drawing time per picture: decides whether step 6 is needed now (P6 > drawing) or
   only for 480 fps (drawing ~2 ms). **P6 avg 5.8 ms against drawing 7.0; P6 longer in 13-18 %
   of tick pictures**: step 4 alone already hides most of P6; step 6 for the rest.

### Critical files

- `src/platform/mod.rs`
- `src/platform/fps.rs`
- `src/platform/kernel32.rs`
- `src/rt.rs`
- `src/platform/d3d9/device.rs`
