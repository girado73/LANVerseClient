# LAN Bash Client

Native Rust client for **LAN Bash**, a server-authoritative pixelart
multiplayer game. The server is Laravel and communicates over WebSockets; the
client renders the world, sends input intent and never decides gameplay
outcomes itself.

## Quick start

The game server is started separately (Laravel project):

```bash
cd ../LANVerseServer && php artisan game:serve --port=8080
```

The client needs the **Reverb app key** in the URL (`/app/<key>`), because
`game:serve` routes the game protocol behind it. The key is the server's
`REVERB_APP_KEY` (`.env`) and is already the default, so a local server works
out of the box:

```bash
# Default: ws://127.0.0.1:8080/app/<default key>
cargo run -- --player-name Alice

# Other host / explicit key (the key in the URL always wins)
cargo run -- --server 192.168.1.20:8080 --player-name Alice
cargo run -- --server ws://192.168.1.20:8080/app/<reverb-key>

# Authenticated play (Sanctum token from POST /api/v1/auth/login)
cargo run -- --server 192.168.1.20:8080 --token "1|…"

# Headless (no window) for CI / debugging – runs at real speed
cargo run -- --headless --max-ticks 600

# See the client without a server (in-memory demo match with bots)
cargo run -- --mock-server

# No window (CI smoke test / debugging)
cargo run -- --mock-server --headless --max-ticks 600

# Render a fixed number of frames (screenshot / smoke test)
cargo run -- --mock-server --max-frames 120

cargo test
cargo clippy --all-targets
```

| environment variable | purpose                                |
|----------------------|----------------------------------------|
| `LAN_BASH_SERVER`    | server endpoint (`host:port`, `ws://host:port/app` or full URL) |
| `LAN_BASH_REVERB_KEY` | Reverb app key appended to `host:port` / `/app` URLs |
| `LAN_BASH_PLAYER_NAME` | display name                         |
| `LAN_BASH_TOKEN`     | auth token forwarded in `hello`        |
| `RUST_LOG`           | `tracing` filter, e.g. `lan_bash_client=debug` |

### When the server is not running yet

The client is meant to be started first: it retries forever with a 2 s cap and
connects as soon as `game:serve` comes up. The HUD shows the endpoint and the
concrete error instead of an empty world. (Startup failures used to fill the
log with "input buffer full" – that is fixed; the buffer is only used while a
session is established.)

> **Server-side blocker (not a client bug).** The server's tick loop runs on
> Reverb's event loop. A blocking call *on that loop* (e.g. a database query for
> login) stalls every snapshot, and the client then trips its 8 s heartbeat and
> reconnects. If you see "heartbeat timeout" while the server is up, look at the
> server process, not here.

## Controls

| input     | keys               |
|-----------|--------------------|
| move      | `WASD` / arrows    |
| jump      | `Space`            |
| dash      | `Left Shift`       |
| interact  | `E`                |
| use item  | `F`                |
| debug HUD | `F1`               |

## Module map

The client grows by **modules, not files**. Each directory below owns one
responsibility and exposes a documented public API:

```
src/
├── core/           ids, Vec2 math, Tick, fixed-step clock
├── config/         CLI/env settings, defaults, reconnect policy
├── protocol/       wire format shared with Laravel + validation
├── models/         server-mirrored world state, GameState, visual state
├── net/            WebSocket transport, session driver, reconnect,
│   ├── transport   WebSocket (tokio-tungstenite) / MessageTransport trait
│   ├── session     async driver: connect → handshake → select! loop → reconnect
│   ├── connection  game-thread handle + connection state machine
│   ├── client      GameClient: validates & applies server truth
│   ├── events      NetEvent stream shared by real and mock drivers
│   ├── latency     RTT estimation for interpolation delay
│   └── mock        in-memory demo/test server
├── input/          sampling, one-shot actions, replay buffer for reconciliation
├── simulation/     tick rate, client prediction & reconciliation
├── interpolation/  snapshot interpolation for smooth remote players
├── camera/         dead-zone follow camera, world↔screen mapping
├── render/         raylib rendering: pixelart pipeline, scene, HUD, sprites
├── platform/       raylib window + keyboard adapter
├── audio/          sound backend trait, mixer with cooldown
├── effects/        confirmed events → sound cues, hit flashes, popups, shake
└── app/            the game loop wiring everything together
```

Dependency rule: lower layers never import higher ones (`net` does not import
`render`, `models` does not import `net`, and only `render` + `platform` touch
raylib).

See [`docs/protocol.md`](docs/protocol.md) for the complete client ↔ server
contract (Laravel side implementation reference).

## Architecture in one picture

```text
Laravel ──WebSocket(JSON)──► session driver (own thread + tokio runtime)
                                   │ NetEvents (channel)
                                   ▼
                     GameClient.update() ──► GameState (server truth)
                                   │
                                   ▼
game loop ── input sample ──► prediction ──► local_override (render only)
                                   │
                                   ▼
                     render (internal 480×270 texture → upscale, nearest)
```

Key invariants:

1. **The server is the truth.** Positions, hp, scores and phase are only
   written from validated snapshots/events.
2. **The client sends intent.** Input messages carry normalized movement and
   one-shot actions – never positions.
3. **Prediction is render-only.** It lives in `GameState::local_override` and
   is discarded whenever authority is lost.
4. **Every network failure is recoverable** (backoff + reconnect) unless the
   server declares it fatal.

## Rendering

The world is rendered into a 480×270 render texture and upscaled with
nearest-neighbour filtering, so pixels stay square at any window size. The HUD
draws at the same internal resolution. Players are drawn procedurally until a
sprite/asset pipeline exists (`render::sprites::draw_player`).

## Known prototype limitations

- Player movement prediction uses a hard-coded `MovementModel` speed; it is
  corrected by every snapshot but should be replaced by server-provided
  movement parameters (or full rollback simulation) for real game modes.
- Remote players are interpolated, not extrapolated (correct, but adds ~RTT/2
  perceived latency).
- Audio uses a `NullAudioBackend`; the asset list is in `audio::backend`.
- No map/collision data yet — the playfield is a fixed 480×270 rectangle.
- The protocol has no compression; JSON is fine for a LAN, CBOR is reserved
  (`protocol::Encoding`).
