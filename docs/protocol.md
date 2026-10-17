# LAN Bash – Client ↔ Server Protocol

This document is the contract between the Rust client and the Laravel game
server. Anything not described here must be treated as *not authoritative* by
the client.

- **Transport:** WebSocket, one JSON object per text frame.
- **Version:** `PROTOCOL_VERSION = 1` (`src/protocol/mod.rs`).
  Any breaking change bumps it; the client refuses to run against a mismatched
  version (`welcome.protocol_version`).
- **Tagging:** every message has `"type"` (snake_case). Server → client
  gameplay events use `"event"` inside a `type: "event"` envelope.

Unknown `type`s and unknown `event`s are **ignored**, so the server can add
messages/events without breaking older clients.

## Connection sequence

```text
client                                          server (Laravel)
   │  WS upgrade  ?auth=<token>                     │
   │ ─────────────────────────────────────────────► │
   │                                                │  validate token (optional)
   │  {"type":"hello", "protocol_version":1, ...}   │
   │ ─────────────────────────────────────────────► │
   │  {"type":"welcome", "player_id":…, "tick_rate":60, "server_tick":0}
   │ ◄───────────────────────────────────────────── │
   │  {"type":"round_state", "phase":"running"}      │
   │ ◄───────────────────────────────────────────── │
   │  {"type":"input", "sequence":1, …}  (every tick)│
   │ ─────────────────────────────────────────────► │
   │  {"type":"snapshot", "tick":…, "players":[…]}   │  ≤ 20 Hz
   │ ◄───────────────────────────────────────────── │
```

The client sends `hello` within milliseconds of opening the socket and
considers the session *connected* only after `welcome`.

### Timeouts and reconnects

| Parameter              | Value                     |
|------------------------|---------------------------|
| Client ping            | every 2 s                 |
| Heartbeat watchdog     | no message for 15 s ⇒ reconnect |
| Reconnect backoff      | 500 ms, 1 s, 2 s … capped at 10 s, 12 attempts |
| Fatal errors           | `error` with `"fatal": true`, or protocol mismatch ⇒ no reconnect |

The server should either answer the client's `ping` or send data at least
every 15 s.

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

Sent once per connection, directly after the socket opens. Duplicate hellos on
the same socket are ignored by both sides.

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
| `sequence`    | monotonic per connection; echo the **highest processed** in `snapshot.acked_input_sequence` |
| `movement`    | normalized intent, length ≤ 1, `{x,y}` in `[-1,1]`              |
| `actions`     | edge-triggered one-shots: `jump`, `dash`, `use_item`, `interact`, `emote` |

Inputs are sent once per client tick (60 Hz by default). They are *intent*:
the server decides position, collisions and the effect of every action.
Dropped inputs are fine; the next one supersedes them.

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
  "match_id": "m-42",           // optional
  "tick_rate": 60,              // server simulation Hz – the client adopts this
  "server_tick": 0,
  "message": "Welcome, Alice"   // optional
}
```

`tick_rate` must be within `1..=240`; the client validates and rejects the
connection otherwise.

### `snapshot` (authoritative world state)

Send at a fixed rate (10–20 Hz is plenty; the client interpolates).

```json
{
  "type": "snapshot",
  "tick": 1204,
  "acked_input_sequence": 812,          // highest input processed from THIS client
  "round": { "round_id": "r-3", "time_left_ms": 42000 },
  "players": [
    {
      "id": "p-17",
      "name": "Alice",
      "position": { "x": 120.5, "y": 90 },
      "velocity": { "x": 0, "y": 0 },
      "facing": { "x": 1, "y": 0 },
      "hp": 100,
      "max_hp": 100,
      "is_alive": true,
      "score": 3,
      "team": "red"
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

Each snapshot is a **full** list of players; players missing from a snapshot
are hidden. The client never predicts other players.

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
{ "type": "event", "event": "score_changed",     "player_id": "p-17", "score": 4, "team": "red" }
```

### `round_state`

```json
{
  "type": "round_state",
  "phase": "countdown",           // connecting | lobby | countdown | running | finished
  "round": "r-3",
  "round_number": 3,
  "countdown_ms": 3000,
  "winner": "red"                 // optional
}
```

The client only accepts gameplay input while `phase == "running"`.

### `pong`

```json
{ "type": "pong", "client_time_ms": 1739184020123, "server_time_ms": 1739184020125 }
```

### `error`

```json
{ "type": "error", "code": "auth_failed", "message": "Invalid token", "fatal": true }
```

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
