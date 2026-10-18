# LAN Bash Client — Handover / Working Notes

> **Status: working against the real server.** Everything below was verified on
> 2026-10-18 against `LANVerseServer` (`php artisan game:serve --port=8080`,
> Reverb `sky7dm0tjxsre1lpqwjg`) unless marked **UNCERTAIN**.
>
> This file was originally a server-side handover. It has been rewritten for the
> Rust client; the few server facts that matter for the client are in §9.

---

## 1. Quality gates (all green at handover)

```bash
cargo fmt -- --check
cargo clippy --all-targets      # 0 warnings
cargo test                      # 102 unit + 6 + 3 + 1 integration tests
```

Manual smoke test against the real server:

```bash
cd ../LANVerseServer && php artisan game:serve --port=8080 &
cargo run -- --headless --player-name RustClient --max-ticks 600
# → handshake complete … phase Countdown → Running … 1 player … tick 600
```

The client can also be started **before** the server: it retries every ≤ 2 s
and connects once `game:serve` is up (verified live; regression test:
`tests/late_server.rs`).

---

## 2. What was fixed in this pass (symptoms → causes)

### 2.1 `Connection refused` loop, "retries exhausted", endless log spam
- **Cause:** `ReconnectPolicy::default()` gave up after 12 attempts (≈ 30 s),
  the status then stayed `Reconnecting { attempt: 6 }` forever, and the session
  driver logged `connection lost` + `waiting before reconnect` per attempt.
- **Fix:** unlimited retries by default (`max_attempts: 0`), backoff capped at
  2 s, new non-terminal status `ConnectionStatus::Retrying`, loud log only for
  the first failure of an outage, everything else at `debug`.
- `GameClient::reconnect()` restarts the driver with the stored hello (for
  `Retrying` / fatal errors).

### 2.2 "input buffer full, dropping oldest unacked sample" 60×/s
**Root cause (found in the GUI client, the most important fix of this pass):**
`Predictor::on_snapshot` was **never called outside unit tests**. That method is
the only place where acknowledged inputs are pruned *and* the only source of
prediction authority, so:
- the replay buffer could only grow → overflow after ~15 s at 300 entries,
- `Predictor::authority` stayed `None` → **local prediction was effectively
  off** (`local_override` always `None`, own sprite lagged by the full RTT).

Fix: `GameClient` keeps the newest applied snapshot and hands it to the game
loop (`take_applied_snapshot`), which calls
`predictor.on_snapshot(&snapshot, &mut input_buffer)` every frame. Verified at
144 fps against the real server: pending inputs stay ~16, 0 overflows, ack
~19/s. Regression test: `acknowledged_inputs_are_pruned_while_playing`
(`tests/integration_net.rs`).

Three further, independent causes, all fixed:
1. **Offline sampling.** The tick loop sampled and buffered inputs even without
   a session. Now it only samples when `GameClient::is_connected()` *and* the
   `welcome` arrived; otherwise buffer/prediction are cleared.
2. **Log spam.** `InputBuffer` counted but logged every drop. It now logs once
   per overflow streak and exposes `take_overflow()` (→ HUD counter).
3. **Sample rate vs. ack rate.** At 144 fps, one sample per tick meant ~7
   samples per 20 Hz ack. The loop now samples at most once per 3 ticks
   (`ACK_WINDOW` ≈ 20 samples/s, above the measured ack rate of ~19/s).
   Buffer default 128 → 300 (≈ 5 s at 60 Hz).
4. **Log flag re-armed every tick.** `take_overflow()` reset the
   once-per-streak flag, so it logged on every tick anyway. The streak now
   ends when an ack drains the buffer.

> **Remaining, server-side:** the Laravel simulation acked at ~14 inputs/s in
> our measurements (`ack 200` after 13 s) and rejects stale sequences, so long
> headless runs can still overflow (= only a prediction-quality symptom, no
> error). Not reproducible at the real 20 Hz snapshot rate.

### 2.3 Wrong default server URL ⇒ HTTP 403
- The old default was `ws://127.0.0.1:8000/app/lan-bash/game`, which `game:serve`
  answers with **403**: the game protocol lives behind `/app/<REVERB_APP_KEY>`.
- New default: `ws://127.0.0.1:8080/app/sky7dm0tjxsre1lpqwjg`.
- `--server` / `LAN_BASH_SERVER` accepts `host:port`, `ws://host:port/app`,
  `http(s)://…` and full URLs; missing key path is appended
  (`LAN_BASH_REVERB_KEY` overrides the built-in key).

### 2.4 A single bad frame could kill the session
- Parsing was strict; any unexpected field/shape produced a `TransportError`
  and wiped the HUD. Now `protocol::codec::parse_frame` decodes the `type`
  first: unknown types → `Unknown` (ignored), broken bodies → logged and
  skipped, Session continues.
- Reverb's `pusher:connection_established` (no `type`) is ignored explicitly.

### 2.5 Status/welcome race, stale state after disconnect
- `welcome` is now reported **before** the `Connected` status, so the game loop
  never sees a connection without a player id.
- Losing the connection clears `GameState` (players, round, phase, prediction,
  interpolation buffer) and shows "Connection lost" once.
- `Connected` is gated on the welcome (`handshake_complete`), so a half-open
  socket cannot send inputs.

### 2.6 Round timer was always 0:00
- Laravel sends `countdown_ms: 0` in every non-countdown `round_state`. The
  client used to write that into the round timer right after it came from the
  snapshot. Now `countdown_ms` only applies during `countdown`.
- HUD format `{:.0}` dropped leading zeros (`1:5`); now `1:05`.

### 2.7 Timeouts and `wss`
- New `connect_timeout` (5 s) and `handshake_timeout` (5 s, "no welcome within
  …"): a server that accepts the socket but never answers `hello` used to sit
  in `Handshaking` until the heartbeat expired.
- Heartbeat 15 s → 8 s (server sends 20 snapshots/s and answers pings).
- `wss://` works (tokio-tungstenite `rustls-tls-webpki-roots`).

### 2.8 Headless loop
- Waits for the handshake (`HEADLESS_CONNECT_TIMEOUT`, 3 s) and warns if it
  never comes.
- Runs at real speed (it used to busy-loop ~3× faster than the server).
- The app owns a runtime for the connection when the caller has none
  (see §6).

### 2.9 Rendering
- Playfield border uses the server world size (`welcome.world`, default
  3200×1800) instead of a hard-coded 480×270.
- Dead players are drawn dimmed with a "KO" tag.
- HUD shows a center banner (status, endpoint, hint) while not in a match, and
  the debug overlay shows world size, ack and overflow count.

### 2.10 "The game flickers badly when I start it"

Measured with `ClientApp::rendered_positions()` (the exact screen coordinates
the renderer draws), at 144 fps pacing. Three causes, all fixed:

1. **The camera swept across the map.** It starts at the world origin and used
   `follow()` (exponential smoothing) from the first frame the player exists:
   measured deltas of **209, 188, 170, … world units per frame** – the whole
   world flew across the screen for ~1 s. Now the camera *snaps* on the first
   frame of a session (and on respawn/reconnect, i.e. when the target is far
   outside the view) and only smooth-follows normal movement.
   `ClientApp::update_camera`.
2. **The local player was drawn on the wrong timeline.** `render_tick` used the
   *client* clock (starts at 0) while snapshots carry the *server* tick (already
   in the thousands). Every player was therefore outside the snapshot buffer
   and got pinned to its oldest entry, jumping whenever the buffer evicted one.
   The client now keeps a tick offset (`server − local`, re-synced on drift) and
   feeds the interpolator the server timeline (`ClientApp::sync_tick_offset`,
   `estimated_server_tick`, `render_tick`).
3. **Prediction was off** (see 2.2): `local_override` was always `None`, so the
   local sprite lagged by a full RTT behind the camera target.

Result: worst per-frame screen movement dropped from **11.6 px to 1.4 px**
(internal 480×270, i.e. ~4 px on a 1440×810 window). Regression tests:
`tests/rendering_motion.rs` (camera snap + smooth drawn motion).

Also fixed while measuring: the mock server's `select!` created a **new**
`sleep` every loop iteration, and a client sending an input per tick kept
winning the branch, so the timer was reset forever and the fake server never
produced a snapshot ("connected but zero players"). It now uses
`tokio::time::interval`.

### 2.11 Bright/dark flicker while playing (presentation, not content)

Follow-up report after 2.10: motion was fine, but the window flickered
bright/dark. Diagnosis:

1. **The content was innocent.** Reading the internal render texture back per
   frame (`LAN_BASH_TRACE_LUMA=1`) showed a constant mean luminance (32.2) for
   the whole session – the game draws a stable image.
2. **The loop swapped the window buffers twice per frame.** `run_gui` used two
   separate `rl.draw()` calls: one for the texture, one for the blit. Each
   `RaylibDrawHandle` is a full `BeginDrawing`/`EndDrawing` cycle, i.e. one
   buffer swap – so every frame presented two different images (the second one
   often a stale/cleared buffer). Measured: **53.5 fps with two swaps vs 109 fps
   with one** at 120 Hz VSync. Now it is a single draw call per frame (texture
   mode inside it), which is the canonical raylib pattern.
3. **VSync and `target_fps = 144` fought each other.** raylib then waits in
   `EndDrawing` (swap) *and* in `SetTargetFPS`, which shows as judder.
   `RenderConfig::target_fps` now defaults to `0` = "VSync paces the loop".

Measured after the fix: 1200 frames in 10.16 s (~118 fps on a 120 Hz display),
no phase/transport errors.

> Diagnostic note: `take_screenshot` is unusable on this Mac (OpenGL-over-Metal
> returns an identical, mostly dark image for every frame, even with moving
> players). `LAN_BASH_TRACE_LUMA=1` reads the render texture instead and is the
> reliable way to tell "content changed" from "presentation is wrong".

### 2.12 "Bob and Alice don't see each other"

Not a networking bug. Both clients receive each other (verified: each snapshot
contains both players, `player_joined` fires, the scoreboard lists both). They
are simply **out of view**:

- world: **3200×1800**, camera viewport: **480×270** → ~2 % of the map visible
- spawn ring: 30 % of the world radius around the centre
  (`World::spawnPointFor`, deterministic per player id), so two players can
  start up to ~1900 units apart
- measured with two real clients: Alice `(2080, 432)`, Bob `(666, 599)` →
  **1414 units apart ≈ 3 screen widths**

Client-side help (display-only, no authority change):
- **minimap** in the HUD (bottom-right) showing the whole map and every alive
  player (local player highlighted),
- **edge markers** for remote players outside the camera
  (`render::scene::draw_offscreen_marker`), so nobody is "just gone".

Spawn proximity is server policy (`config/lanbash.php` world size, spawn ring);
if players should start close together, that has to change on the server.

### 2.13 Pre-existing test failure
`headless_loop_keeps_an_explicit_mock_connection` failed on the untouched
baseline. Root cause: plain `cargo test` provides no Tokio runtime, so the
mock connection future was never polled ("connected", but zero snapshots).
Covered in §6; the test now exercises the real code path.

---

## 3. Protocol — verified against the server

`docs/protocol.md` is the contract; it was updated with the real payloads:

| frame | verified field details |
|---|---|
| `welcome` | `protocol_version`, `player_id` (`p-<id>`), optional `session_id`/`match_id`/`message`, `tick_rate`, `server_tick`. **No** `world`/`move_speed` yet. |
| `snapshot` | `tick`, `acked_input_sequence` (per player, monotonic), `round {round_id, time_left_ms}`, `players[]` with `team: null` in FFA, `velocity`, `facing`. 20 Hz. |
| `round_state` | `phase` + `countdown_ms` always; `round`/`round_number` only once a round exists; `countdown_ms: 0` outside the countdown. |
| `event` | `player_joined`, `player_left`, `player_hit`, `item_picked`, `score_changed` (no `team` in FFA). |
| `error` | `code`, `message`, `fatal`; codes listed in `protocol.md`. |
| pusher | `{"event":"pusher:connection_established",…}` arrives first and is ignored. |

Server-side input handling: `dash`, `jump`, `interact`(= attack) work;
`use_item`/`emote` are accepted but ignored.

---

## 4. Tests worth knowing

| file | what it proves |
|---|---|
| `tests/integration_realtime.rs` | real `WsTransport` + session against a fake `game:serve` on a random port: handshake, snapshots, ping/pong; `handshake_timeout` on a silent server; reconnect after the server appears. |
| `tests/rendering_motion.rs` | camera snaps instead of sweeping; drawn players move < 4 px/frame. |
| `tests/late_server.rs` | exact reported scenario: client runs with no server for 500 ms → connects, gets `welcome`, player and round timer once the server starts. |
| `tests/integration_net.rs` | mock-server end-to-end (handshake, acks, prediction, headless loop). |
| `src/protocol/message.rs` (tests) | every Laravel payload parses verbatim; pusher frames are not errors. |
| `src/protocol/validation.rs` (tests) | `world`/`move_speed` sanity. |
| `src/net/client.rs` (tests) | round timer not clobbered, state reset on disconnect, toast once. |

The real-socket tests need a Tokio runtime on the test thread:
`build_tokio_runtime()` + `enter()` (see the note in §6).

---

## 5. Files changed (map)

```
src/app/loop_runner.rs     runtime for the session, sampling gate, ACK_WINDOW,
                           real-speed headless, HUD server URL
src/app/runtime.rs         (unchanged) build_tokio_runtime()
src/config/settings.rs     new default URL/port/key, retries forever,
                           connect/handshake timeouts, URL normalization
src/net/session.rs         lenient frames, welcome-before-status, silent log,
                           Retrying, timeouts, type-erased transport factory
src/net/connection.rs      ConnectionStatus::Retrying (+ is_retrying)
src/net/transport.rs       wss, upgrade timeout, MessageTransport for Box<dyn _>
src/net/mock.rs            fake server on its own thread (no runtime races)
src/net/events.rs          pusher/unknown frames are silent
src/net/client.rs          welcome ordering, session reset, ack mirroring,
                           reconnect(), round_state rules,
                           take_applied_snapshot() for reconciliation
src/protocol/codec.rs      NEW lenient frame decoder
src/protocol/message.rs    Welcome.world/move_speed, Snapshot.world, defaults
src/protocol/validation.rs finite world/speed checks
src/models/world.rs        Laravel defaults (team null, missing hp/facing)
src/models/game_state.rs   WorldBounds, move_speed, ack/overflow diagnostics
src/input/buffer.rs        counted overflow, one log line per streak
src/input/mod.rs           ensure_sequence_after(), clear_pending_actions()
src/render/{hud,scene,sprites}.rs  banner, world border, KO, timer format
src/render/pipeline.rs            mean_luma() diagnostic readback
src/platform/desktop.rs           VSync paces the loop (no double frame cap)
tests/rendering_motion.rs         NEW camera snap + smooth drawn motion
docs/protocol.md           rewritten from the real implementation
README.md                  setup with app key, env vars, late-start behaviour
```

`Cargo.toml`: `tokio-tungstenite` now enables `rustls-tls-webpki-roots`.

---

## 6. Rules that save hours (client side)

1. **The connection future needs a Tokio runtime to be polled.** The session
   driver creates and polls it; without a runtime it silently never completes
   (symptom: status `Connected`, zero snapshots, tick 0). `ClientApp` therefore
   owns a runtime when the caller has none. Tests: `build_tokio_runtime()` +
   `enter()`; **not** a current-thread runtime around a blocking loop.
2. **A multi-thread runtime may not be dropped inside `block_on`.**
   `new_current_thread()` runtimes have no workers and can be dropped anywhere;
   multi-thread ones panic ("cannot drop a runtime where blocking is not
   allowed"). `ClientApp` keeps its runtime as a field and drops it on its own
   stack.
3. The game loop owns its runtime; the driver thread has its own. Never share
   one runtime between the game thread and the driver.
4. **Never write `0` from `round_state.countdown_ms` into the timer.**
5. **Sequences must stay above the last ack** after a reconnect
   (`InputPipeline::ensure_sequence_after`).
6. A full 403 on the WebSocket is almost always the missing `/app/<key>`.
7. `RUST_LOG=lan_bash_client=trace` shows every frame direction; the session
   logs the URL on the first failure of an outage.

---

## 7. Open items (client)

- [ ] **Mouse/aim**: only `interact` (= attack) exists; no aiming or cursor.
- [ ] **Teams**: `team` is parsed but not rendered (scoreboard is global).
- [ ] **Items**: snapshots do not include item entities; nothing is drawn.
- [ ] **Audio**: `NullAudioBackend` only, no assets.
- [ ] **Sprite pipeline**: procedural rectangles only.
- [ ] **Map/collision data**: border from `world`, no obstacles (server has
      none yet either).
- [ ] **Server-provided `move_speed`**: prediction uses the server default
      (220 u/s, hard-coded in `MovementModel::default`) until the welcome
      announces it. Exact prediction needs `welcome.move_speed`.
- [ ] **Retry button**: `GameClient::reconnect()` exists, no UI/ key binding.
- [ ] **Interpolation**: `render_tick` uses the *client* tick counter, which is
      only loosely related to the server tick (works, but a server-tick based
      estimate would be more precise).
- [ ] **CI**: no GitHub workflow (`fmt`, `clippy`, `test` are ready to be wired).
- [ ] **`cargo test` headless caveat**: the headless integration test needs the
      multi-thread runtime on purpose (see §6.2); if it is ever rewritten for a
      current-thread runtime it must not block the test thread.

## 8. Open items that are server-side (found while testing, not fixed here)

- Simulation acked only ~14 inputs/s while the client sent 60/s (it applies
  one movement step per acked input and drops stale ones), measured against
  `game:serve` on 2026-10-18 (`ack 55` after ~3 s of `input`s).**UNCERTAIN**
  whether that is a load artefact or a deliberate rate limit – check
  `World::applyInput` / the tick/snapshot loop on the server side.
- Tick loop runs on Reverb's event loop; a blocking call there stalls all
  snapshots (client then trips the 8 s heartbeat and reconnects – correct
  client behaviour).

---

## 9. Server facts the client depends on

- `php artisan game:serve --port=8080` starts Reverb **with the game router**;
  `reverb:start` alone does not speak the game protocol.
- Endpoint `ws://<host>:8080/app/<REVERB_APP_KEY>`.
- Auth: Sanctum token in `hello.token`; profile name wins over `player_name`.
  Guest play = no token.
- 60 Hz simulation, 20 Hz snapshots, 3 s countdown, 180 s rounds, respawn 2.5 s.
- Default key `sky7dm0tjxsre1lpqwjg` (`.env` → `REVERB_APP_KEY`).
