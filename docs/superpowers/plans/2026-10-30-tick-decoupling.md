# Even frame times in missions: the logic tick and the picture apart

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or
> superpowers:executing-plans. Steps use checkbox (`- [ ]`) syntax.

**Goal:** in missions every picture follows the one before after the same time. No picture pays
for a whole 40 ms logic tick. The game's logic stays exactly as the original: same 40 ms steps,
same results, sound, input and saves unchanged. It must run on any Windows 11 PC.

**In short:** the game still ticks and draws on its main thread, exactly as today. What changes
is when a finished picture is shown. Each picture goes into a small queue on the GPU and is
shown on an even schedule. Each picture is drawn for the moment it will be shown. The tick's
cost is spread over all the pictures of a 40 ms period instead of landing on one.

**Decision (user, 2026-10-30): design (a), the tick on a second thread. Design (d) below is
rejected: its added latency (about one tick) is not acceptable.** Steps 0 and 1 below are done
first for either design; the steps for (a) are planned from step 1's findings.

Sources: `.superpowers/perf/2026-10-30-mission-profile.md`, `.superpowers/perf/task-p1-report.md`
(git-ignored measurement notes).

## 1. How a mission frame runs today

game_frame (0x45e360, hooked by `fps.rs`) once per picture, with no Sleep (variableUpdate 1):

1. 0x45ecc8..0x45ed07: some per-frame calls, then real_clock_update (0x534270): the real clock
   proj_data+0x360 advances by timeGetTime, in whole ms.
2. Number of ticks n = (real clock - game time) / 40, between 0 and 3. If it is more than 3, the
   clock is pulled back (clock_set 0x4113f0). Frame step +0x4cc = 40 * n.
3. The tick loop 0x45ed8b..0x45eea3, n times. It calls the 7 `TICK_PARTS`:
   - game_clock_tick 0x534e70 (game time += 40)
   - 0x457640(0,0,0,0,0): calls kb_read, so the keyboard is read at the tick
   - 0x4a5620 and 0x4c49d0 (world update)
   - 0x449430(game-time delta)
   - 0x44c300 on +0x4438
   - 0x413c60: its 0x413a70 calls mouse_update 0x4477d0, so the mouse and the aim are read at
     the tick

   All input is therefore read at the tick, 25 times a second. Cost: 5.3-8.9 ms per tick.
4. After the ticks: vtable +0x4c, the skip_frames counter (0x62352c/0x623528), frame_time_set
   0x43be30, envelopes_update 0x44d9f0.
5. The drawing: per player viewport (count [+0x1060]) from 0x45efaf to 0x45fd70: the camera
   (cam_set 0x5491e0), node_draw/set_world, effects_draw, trail_draw, the HUD (hud_frame
   0x43c210, whose hud_draw also calls each element's update, vtable +8), the renderer's queue
   (render_mesh/render_flush on 0x675d40), BeginScene/EndScene/Present (frame_begin 0x46f590,
   0x46f900, 0x46fb90) and the listener commits (CommitDeferredSettings, twice a picture).
6. 0x548bf0/0x548c10 (overlay 0x71e690), and in-mission menus (0x48f240, 0x4dfdc0, which
   changes settings at proj+0x35c). The Sleep at 0x45fe7c is skipped with variableUpdate.

**What fps.rs does.** It adds no pictures; the game itself draws one per game_frame. fps.rs only
changes what each picture shows. The camera, about 470 node places, particles and trails are
blended between the last two ticks by alpha = (real clock - game time) / 40. Each value is
written for the one drawing call and then put back. Alpha comes from the whole-ms clock, which
is coarse at high fps.

**What the drawing reads that the tick writes:** the objects and their model nodes (place,
rotation, scale); the camera object (+0x1cc/+0x1a8); particle emitters (+0xc/+0x10), trail rows
([+0xac]), the light list (0x661d30), water, radar, the effect manager (0x72a780), HUD state,
proj_data timers; the heap (the tick allocates and frees objects); managed textures and meshes
the tick may create.

**What the drawing writes:** the renderer 0x675d40 (draw queue +0x4c4c4, world/view
+0x46574/+0x465b4, the D3D state caches: SetTexture cache +0x46ac0, render-state caches
0x47a020/0x47db50); the dynamic VB/IB ring (NOOVERWRITE positions) and their guest lock copies
(350-510 KB copied out per picture); node world places +0x40; the particle arrays (rebuilt each
picture); HUD element updates and in-mission menu state (logic-like side effects that must not
be thrown away); DirectSound listener commits.

**Why frame times are uneven.** A picture without a tick takes 3.7-7.9 ms, a picture with a
tick 9-15.5 ms. 25 times a second one picture is 2-3x longer, and that spike is what the user
sees.

## 2. The candidates

**(a) The tick on a second thread, in parallel with the drawing, which reads a snapshot.**
Possible in principle: generated code takes its memory base from `Cpu::mem`, so a second 4 GB
image ("world B") could run the same code, kept equal to world A each tick by copying the pages
written since the last sync (write watch, as rustschweine's `Shadow`; the reservation already
has `MEM_WRITE_WATCH`). It needs: `mem()` per thread; our guest heap, VirtualAlloc and critical
sections per world; every import used by the drawing sorted into allowed, no-op (DirectSound)
or refused (D3D Create*: the picture is dropped through `Cpu::stop`); a device lock plus
D3DSBT_ALL state blocks around a world-B picture; DISCARD instead of NOOVERWRITE on the first
dynamic-buffer lock after a world switch; the turn lock reworked for truly parallel guest
threads; and the tick must not call D3D while the drawing holds the device (unknown yet, step 1
measures it). Gain: the tick leaves the main thread's critical path, about +15-20 % fps, no
extra latency. Risks: many races (CRT heap metadata copied mid-update, refcounts, unbalanced
AddRef/Release when a picture is dropped); the drawing's logic side effects; the R6025-type
races between guest threads multiply. Effort: weeks.

**(b) Pictures as transactions while the tick runs (rustschweine).** In Hogs of War the main
thread was idle between steps, so extra pictures were drawn in that idle time. Gun Metal's main
thread is 89-96 % busy and already draws every frame; drawing during the tick needs another
thread and a frozen view of memory, which is (a). Reusable: the write-watch page shadow
(`Shadow::sync/restore`) for (a)'s world sync, and the lesson that drawing writes game state.

**(c) Spreading the tick over several pictures.** Not workable: guest code cannot be stopped in
the middle of the tick and drawn from (the world is half updated); drawing writes values the
rest of the tick may read (node world places +0x40, HUD), which would change the order against
the original; splitting at the 7 part boundaries helps only if the cost is spread over the parts
(most of it is probably the world update; step 1 measures it).

**(d) Pacing: show pictures evenly, draw each for the moment it is shown. Recommended.**
The main thread works exactly as today. Each finished picture is copied into a GPU slot
(StretchRect, which also resolves the 2x MSAA). Our own additional swap chain shows the slots at
even times, on a grid with period P = the measured average time per picture. Showing happens on
the main thread at safe points: when an import returns (`on_event`), at guest loop heads (a
timer thread sets a `POLL` bit; `rt_yield` then shows the due picture), and in our wait at the
end of game_frame. So a picture can be shown in the middle of the tick, and only the main
thread ever calls D3D. Content time (the moment a picture shows) = display time - L, where L
covers the slowest picture (one with a tick, about 12-16 ms now); fps.rs's alpha uses that
content time from a high-resolution clock. Logic: untouched; input is still read at the tick.
Costs: about +5-9 ms latency on top of today's one tick of interpolation lag; (L/P + 2) slots
in VRAM (e.g. about 4 x 15 MB at 2560x1440); two full-screen copies per picture (about 0.1 ms
of GPU); about 3 % fps margin for the schedule. It does not make the average faster: 480 fps
still needs the recompiler wins; (a) stays the later option. Effort: about a week.

## 3. The plan (d), in order

### Step 0: catch the R6025 (independent; first because pacing changes timing)

- [ ] Hook `__amsg_exit` (0x588d97). For code 25 (pure virtual call), log: the thread (main or
      which guest thread and its start address), the return-address chain (walk ebp, names from
      symbols.txt), `LAST_CALL`, and per thread its last import. Then exit as before.
- [ ] Test: a unit test for the chain walker on a made-up stack. Run 10 cold starts to the
      mission (the CLAUDE.md script; back up and restore the user's saves) and record any hit
      in PLAN.md.
- [ ] Commit.

### Step 1: frame-time log and the baseline

- [ ] `GUNMETAL_FRAMES=<file>` (and a 2 s summary in the log). Per picture: QPC time of the
      shown Present and its interval to the one before; time to produce the picture; ticks in
      that frame and time spent in tick parts (`IN_TICK` already exists).
- [ ] Summary per 2 s and at the end: average, 99th percentile, maximum; separately for
      intervals with and without a tick; fps.
- [ ] Finding aids, logged once: imports called inside tick parts by name and count (does the
      tick call D3D at all? needed for (a)); time per tick part; pages written per tick and per
      picture (GetWriteWatch on the heap and .data).
- [ ] Tests: percentile and summary maths on fixed inputs.
- [ ] Run the profile script 3 times; write the baseline into PLAN.md. Expected: intervals with
      a tick 2-3x the others, 99th percentile about the tick frame.
- [ ] Commit.

### Step 2: content time from a fine clock

- [ ] Hook real_clock_update (0x534270). After the original runs, keep (real clock ms, QPC).
      fps.rs `alpha()` computes content time = real clock + (QPC now - QPC at the update), in
      fractions of a ms. Clamp as now. Logic unchanged: only the drawing reads it.
- [ ] Tests (`tests/fps.rs`): alpha from (clock, QPC pair, now) at tick boundaries, pause
      (clock not moving), and a clock pulled back.
- [ ] Seen: the mission looks the same; the frame log shows content steps no longer in whole ms.
- [ ] Commit.

### Step 3: our own presenting swap chain (still showing at once)

- [ ] `d3d9/present.rs`: CreateAdditionalSwapChain on our window at its client size; slots =
      non-MSAA render targets in the back buffer's size and format. At the game's Present (no
      rectangles from the game; otherwise today's path): GUNMETAL_SHOT; StretchRect from the
      back buffer into the slot (resolve); `clear_back_buffer` as now; right away from the slot
      onto the swap chain's back buffer, letterboxed by the destination rectangle, sides cleared
      black (replaces the GDI bars on this path); the swap chain's Present.
- [ ] Release the swap chain and slots before Reset and on loss; recreate after. Recreate on a
      window size change or Alt+Enter.
- [ ] Switch: registry `Pacing` (DWORD, game key) = 0, or `GUNMETAL_PACING=0`, gives today's
      direct Present.
- [ ] Tests: the letterbox rectangle maths (pure). Scripted: menus, mission, `lose*2000`,
      blur/focus/minimise; Alt+Enter by hand. Frame log: average within +-3 % of step 1.
- [ ] Commit.

### Step 4: the schedule (pure Rust, not wired)

- [ ] `fps/pace.rs`. Input: per picture, when it started and finished. Output: content time
      c_k, display time s_k = c_k + L, and when the next picture may start.
      P = average time per picture over about 1 s plus 3 %, never below a frame limit if one is
      set (this also gives PLAN's "frame-rate limit setting"). L = 99th percentile of
      (finish - c_k) over 2 s plus 1 ms. Behind by more than L: the grid restarts at now (one
      visible step, logged). Queue full: wait.
- [ ] Tests: steady 5 ms pictures with a 12 ms one every 40 ms give display intervals equal
      within 0.1 ms; slow-down and recovery; pause (no pictures for 500 ms) restarts; queue
      bound.
- [ ] Commit.

### Step 5: pictures shown on time

- [ ] Wire step 4 into step 3 while `alpha()` is active (mission, variableUpdate, Smooth on,
      pacing on). At the game's Present: resolve into a free slot labelled with s_k. Otherwise
      everything shows at once and the queue is emptied.
- [ ] Safe points that show due slots (closing the scene first if the game is inside
      BeginScene/EndScene, opening it again after; tracked in begin_scene/end_scene):
      (1) after every import in `on_event`, one relaxed load of a "due" flag;
      (2) `rt_yield`: a timer thread with a high-resolution waitable timer sets the due flag and
      a `POLL` bit at s_k (`POLL` becomes two bits, turn waiting | present due, so
      `turn_take/turn_give` no longer overwrite it);
      (3) the end of our game_frame hook, which also waits (timer, short spin at the end) until
      the next picture may start.
- [ ] fps.rs alpha uses c_k for the picture being drawn.
- [ ] The game's pause, focus loss, minimising and device loss: the queue is shown at once or
      dropped, and pacing resumes after 2 steady pictures.
- [ ] Measure with step 1's log, 3 runs: 99th percentile of the intervals <= 1.25 x average;
      maximum <= 2 x average (outside loading); average interval with a tick / without <= 1.15
      (now 2-3); average fps >= 95 % of the baseline; content step per picture equals the
      display step within 0.2 ms; L logged (expected 10-16 ms).
- [ ] By eye and hand (the user): turning and walking are smooth, the aim feels the same,
      in-mission menu and Alt+Tab work.
- [ ] Commit.

### Step 6: hand-over

- [ ] CLAUDE.md (`fps.rs`, `d3d9/present.rs`, `Pacing`, `GUNMETAL_FRAMES`) and PLAN.md
      (results, latency, the R6025 status, (a) as the later option with step 1's findings).
- [ ] Commit.

## 4. What can go wrong

- **Present inside a scene or during a lock.** Showing only outside a scene (End/Begin around
  it); StretchRect and swap-chain Present change no device state. A blocking Present (GPU queue
  full) inside the tick only makes that tick longer.
- **`POLL` shared with the turn lock.** Two bits, each cleared only by its owner; otherwise a
  lost bit means a late picture.
- **Reset and loss.** Release before Reset, empty the queue; scripted `lose*2000` covers it.
- **Pauses.** The game's clock stops (app_deactivate, menus): content time stops too, the queue
  is emptied, no stale pictures.
- **Latency.** +L. If it is felt: a smaller margin, or `Pacing`=0.
- **VRAM.** Slots grow with fps x L (about 8 at 480 fps); capped, and the schedule waits
  instead.
- **Timing changes provoke the loading race (R6025).** Step 0 makes it visible. Pacing is active
  only in missions, not during loading.

## 5. The R6025 crash

"R6025 pure virtual function call" is _purecall through `__amsg_exit(25)`: a virtual method was
called on an object that is being built or destroyed, the classic race between two threads over
an object's lifetime. At mission load several guest threads are alive: the loader thread
0x4eadf0, stream_thread 0x469300 (calls its stream's callback +0x3c when woken) and the music
workers. They are woken by our host-event forwarders (`dsound::host_event`), whose timing differs
from real DirectSound after Stop or Release. Our threads change turns only at imports and loop
heads; a faster main thread (indirect dispatch, -17 % per picture) changes how they interleave.
That fits 1 crash in 9 runs after a change that only affected timing. Design (d) runs no guest
code in parallel and adds no guest threads, so it does not need this fixed first; step 0 catches
it with a call chain. Design (a) would need it found and fixed first.

### Critical files

- `src/platform/fps.rs`
- `src/platform/d3d9/device.rs`
- `src/platform/mod.rs` (on_event, turn lock, POLL)
- `src/rt.rs` (rt_yield, POLL)
- `src/lift.rs` (loop-head poll)
