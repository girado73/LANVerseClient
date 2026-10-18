//! # LAN Bash Client
//!
//! Native Rust client for **LAN Bash**, a server-authoritative pixelart
//! multiplayer game served by LANVerseGame over WebSockets.
//!
//! ## Architecture
//!
//! The crate is organised as feature modules, each with a single
//! responsibility. The arrows show *allowed* dependencies (lower layers never
//! import higher ones):
//!
//! ```text
//!                      ┌──────────┐
//!                      │   app    │  game loop, orchestration
//!                      └────┬─────┘
//!          ┌────────────────┼───────────────┬──────────────┐
//!          ▼                ▼               ▼              ▼
//!      ┌────────┐      ┌──────────┐    ┌─────────┐    ┌────────┐
//!      │ render │      │ platform │    │  audio  │    │  hud   │
//!      └───┬────┘      └────┬─────┘    └────┬────┘    └────────┘
//!          │                │               │
//!          ▼                ▼               ▼
//!  ┌───────────────┬──────────────┬──────────────┬───────────────┐
//!  │   camera      │    input     │   effects    │  simulation   │
//!  └───────┬───────┴──────┬───────┴──────┬───────┴───────┬───────┘
//!          │              │              │               │
//!          ▼              ▼              ▼               ▼
//!      ┌──────────────────────────────────────────────────────┐
//!      │        interpolation · models · protocol             │
//!      └──────────────────────────────────────────────────────┘
//!                                   │
//!                                   ▼
//!                            ┌────────────┐
//!                            │    core    │
//!                            └────────────┘
//!
//!      net: protocol + models + core  (no raylib!)
//! ```
//!
//! | module          | responsibility                                                  |
//! |-----------------|-----------------------------------------------------------------|
//! | `core`          | ids, math, ticks, fixed-step clock                               |
//! | `config`        | CLI/env settings, defaults, reconnect policy                     |
//! | `protocol`      | wire format shared with the server (JSON messages), validation    |
//! | `models`        | server-mirrored world state + purely local visual state          |
//! | `net`           | WebSocket transport, session driver, reconnect, game facade      |
//! | `input`         | input sampling, one-shot actions, replay buffer                  |
//! | `simulation`    | fixed tick rate, client prediction/reconciliation                |
//! | `interpolation` | smooth rendering between snapshots                               |
//! | `camera`        | dead-zone follow camera, world↔screen mapping                    |
//! | `render`        | raylib rendering: pixelart pipeline, scene, HUD                  |
//! | `platform`      | raylib window + keyboard adapter                                 |
//! | `audio`         | sound backend abstraction, mixing/rate limiting                  |
//! | `effects`      | confirmed gameplay events → local FX (flash, popups, sound cues) |
//! | `app`           | the game loop that wires everything together                     |
//!
//! ## Invariants
//!
//! 1. **The server is the truth.** `models::GameState.players` is only written
//!    from validated snapshots. `local_override` is render-only prediction.
//! 2. **The client sends intent.** `protocol::message::InputMessage` carries
//!    normalized movement + one-shot actions, never positions.
//! 3. **Only `net` talks JSON, only `render`/`platform` talk raylib.**
//! 4. **Every network failure is recoverable** unless the server says otherwise
//!    (`ServerError { fatal: true }` / protocol mismatch).
//!
//! ## Running
//!
//! ```sh
//! cargo run -- --server ws://127.0.0.1:8000/app/lan-bash/game --player-name Alice
//! cargo run -- --mock-server --player-name Tester   # in-memory demo server
//! cargo run -- --headless                           # no window, for CI
//! ```

pub mod app;
pub mod audio;
pub mod camera;
pub mod config;
pub mod core;
pub mod effects;
pub mod input;
pub mod interpolation;
pub mod mode_cache;
pub mod models;
pub mod net;
pub mod platform;
pub mod protocol;
pub mod render;
pub mod simulation;

pub use app::{ClientApp, ExitReason, RunMode};
pub use config::ClientConfig;
pub use models::GameState;
