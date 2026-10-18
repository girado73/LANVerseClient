# LAN Bash – Client ↔ Server Protocol

This document is the contract between the Rust client and the game server
(`LANVerseGame`). Anything not described here must be treated as *not
authoritative* by the client. The server-side canonical version, including the
REST control plane, lives in `LANVerseGame/docs/protocol.md`.

- **Transport:** WebSocket, one JSON object per text frame.
- **Endpoint:** `ws://<host>:8080/` (root path). The former `/app/<key>` path is
  still accepted by the server as an alias.
- **Version:** `PROTOCOL_VERSION = 1` (`src/protocol/mod.rs`).
  A mismatched `welcome.protocol_version` fails the connection.
- **Tagging:** every message has `"type"` (snake_case). Server → client
  gameplay events use `"event"` inside a `type: "event"` envelope.

Unknown `type`s and unknown `event`s are **ignored**, so the server can add
messages/events without breaking older clients.

### Tolerated non-game frames

The server sends a `pusher:connection_established` event on every socket before
the game protocol starts (kept for clients of the former transport; it carries
no game state):

```json
{ "event": "pusher:connection_established", "data": "{\"socket_id\":\"1.2\"}" }
```

Frames without a `type` are treated as "not protocol" and ignored. They never
kill the session and never reach the game state.

## Connection sequence

```text
client                                          server
   │  WS upgrade  /                                │
   │ ─────────────────────────────────────────────► │
   │  {"event":"pusher:connection_established"}     │  (ignored by the client)
   │ ◄───────────────────────────────────────────── │
   │  {"type":"hello", "protocol_version":1, ...}   │
   │ ─────────────────────────────────────────────► │
   │  {"type":"welcome", "player_id":…, "tick_rate":60, "server_tick":0}
   │ ◄───────────────────────────────────────────── │
   │  {"type":"round_state", "phase":"countdown"}   │
   │ ◄───────────────────────────────────────────── │
   │  {"type":"input", "sequence":1, …}  (every tick)│
   │ ─────────────────────────────────────────────► │
   │  {"type":"snapshot", "tick":…, "players":[…]}   │  ≤ 20 Hz
   │ ◄───────────────────────────────────────────── │
```

The client sends `hello` within milliseconds of opening the socket and
considers the session *connected* only after `welcome`.

### Timeouts and reconnects

| Parameter              | Value                                      |
|------------------------|--------------------------------------------|
| Connect timeout        | 5 s (+ 10 s hard limit inside the transport) |
| Handshake timeout      | 5 s without `welcome` ⇒ retry              |
| Client ping            | every 2 s                                  |
| Heartbeat watchdog     | no message for 8 s ⇒ reconnect             |
| Reconnect backoff      | 0.5 s, 1 s, 2 s … capped at 2 s            |
| Max attempts           | unlimited (`0` = forever) – a client may start before the server |
| Fatal errors           | `error` with `"fatal": true`, or protocol mismatch ⇒ no reconnect |

The server answers `ping` with `pong` and sends snapshots at 20 Hz, so a healthy
link never trips the watchdog.

## Client → Server

### `hello`

```json
{
  "type": "hello",
  "protocol_version": 1,
  "player_name": "Alice",
  "token": "1|Xyz…",            // optional (Sanctum token, guest = omitted)
  "match_id": "m-42",           // optional, server auto-assigns otherwise
  "client_version": "0.1.0"
}
```

Sent once per connection, directly after the socket opens. With a valid token
the server uses the account's profile name and ignores `player_name`
(`GameIdentity::displayName()`). Duplicate hellos on the same socket are
ignored by the server.

### `input` (from `AGENTS.md`)

```json
{
  "type": "input",
  "client_tick": 18420,
  "sequence": 815,
  "movement": { "x": 1, "y": 0 },
  "actions": ["jump"]           // omitted when empty
}
```

| field         | meaning                                                         |
|---------------|-----------------------------------------------------------------|
| `client_tick` | client's estimate of the current server tick                    |
| `sequence`    | monotonic per player; the server ignores `sequence <= acked_input_sequence` |
| `movement`    | normalized intent, length ≤ 1, `{x,y}` in `[-1,1]`              |
| `actions`     | edge-triggered one-shots: `jump`, `dash`, `use_item`, `interact`, `emote` |

Inputs are *intent*: the server decides position, collisions and the effect of
every action. Dropped inputs are fine; the next one supersedes them.

Server mapping (current implementation): `dash`, `jump` and `interact` are
executed; `interact` is the attack. `use_item` and `emote` are accepted but
ignored.

**Sequence rules for the client.** After a reconnect the counter must continue
above the last acknowledged sequence, otherwise every input is discarded.

### `ping`

```json
{ "type": "ping", "client_time_ms": 1739184020123 }
```

## Server → Client

### `welcome`

```json
{
  "type": "welcome",
  "protocol_version": 1,
  "player_id": "p-17",
  "session_id": "s-…",          // optional
  "match_id": "lan-20261017",   // optional
  "tick_rate": 60,              // server simulation Hz – the client adopts this
  "server_tick": 0,
  "message": "Welcome, Alice"   // optional
}
```

`tick_rate` must be within `1..=240`; the client rejects the connection
otherwise. `world` (`{width, height}`) and `move_speed` are accepted if a future
server adds them; today they are absent and the client falls back to the
server defaults (world 3200×1800, speed 220).

### `snapshot` (authoritative world state)

Sent at 20 Hz (`LANBASH_SNAPSHOT_RATE`).

```json
{
  "type": "snapshot",
  "tick": 1204,
  "acked_input_sequence": 812,          // highest input processed from THIS client
  "round": { "round_id": "lan-20261017", "time_left_ms": 179000 },
  "players": [
    {
      "id": "p-17",
      "name": "Alice",
      "position": { "x": 768.62, "y": 630 },
      "velocity": { "x": 0, "y": 0 },
      "facing": { "x": 1, "y": 0 },
      "hp": 100,
      "max_hp": 100,
      "is_alive": true,
      "score": 0,
      "team": null                      // null in free-for-all
    }
  ]
}
```

**Requirements the client validates** (invalid snapshots are dropped):

- player ids unique within a snapshot (≤ 512 players),
- all floats finite,
- `0 < max_hp`, `hp ≤ max_hp`,
- `|position| ≤ 1e6`,
- non-decreasing `tick` (older snapshots are ignored).

`acked_input_sequence` is **per player** and monotonic. Even an out-of-order
snapshot's ack is applied, because it is never stale on its own.

Each snapshot is a **full** list of players; players missing from a snapshot
are hidden. The client never predicts other players. Positions are rounded to
two decimals – the client interpolates anyway.

### `event`

Discrete things that happened. The client reacts with sound/FX; it never
creates these events itself.

```json
{ "type": "event", "event": "player_joined",     "player_id": "p-18", "name": "Bob" }
{ "type": "event", "event": "player_left",       "player_id": "p-18" }
{ "type": "event", "event": "player_hit",
  "attacker": "p-17", "victim": "p-18", "damage": 12,
  "remaining_hp": 88, "position": { "x": 140, "y": 92 } }
{ "type": "event", "event": "item_picked",       "player_id": "p-17", "item_id": "i-9" }
{ "type": "event", "event": "score_changed",     "player_id": "p-17", "score": 4 }
```

`score_changed` omits `team` outside team modes.

### `round_state`

```json
{
  "type": "round_state",
  "phase": "countdown",           // connecting | lobby | countdown | running | finished
  "round": "lan-20261017",        // omitted while no round exists
  "round_number": 1,              // omitted while no round exists
  "countdown_ms": 3000,           // 0 outside the countdown
  "winner": "red"                 // optional
}
```

The client only accepts gameplay input while `phase == "running"`.

`countdown_ms` is authoritative **only** during `countdown`; outside of it the
server sends `0`, which the client ignores (the round timer comes from the
snapshots).

### `pong`

```json
{ "type": "pong", "client_time_ms": 1739184020123, "server_time_ms": 1739184020125 }
```

### `error`

```json
{ "type": "error", "code": "auth_failed", "message": "Invalid token", "fatal": true }
```

Codes: `protocol_mismatch`, `auth_failed`, `match_not_found`, `match_full`,
`invalid_hello`, `invalid_message`, `bad_request`, `server_error`.

`fatal: true` stops reconnecting and shows the error in the HUD.

## Client-side interpretation rules

1. `players` in snapshots is the **only** source of positions, hp and scores.
2. The local player's render position is predicted (`simulation::prediction`)
   and corrected whenever a snapshot with a newer `acked_input_sequence` or
   `tick` arrives. Corrections never modify server data.
3. Remote players are rendered by interpolating between the two snapshots that
   bracket `current_tick - latency/2`, i.e. ~half an RTT in the past. No
   extrapolation is performed.
4. If the client falls behind by more than 8 ticks in one frame the backlog is
   dropped; the server state re-syncs it automatically.
5. When the connection drops, all session-scoped state (player id, players,
   round, prediction, interpolation buffer) is cleared. A ready-state frame
   from the game loop never runs against stale data.
