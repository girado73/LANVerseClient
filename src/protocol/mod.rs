//! Wire protocol between the client and the Laravel game server.
//!
//! # Design
//!
//! * One envelope per WebSocket text frame, JSON encoded.
//! * Every message is an internally tagged enum (`"type": "…"`), so the
//!   Laravel side can simply `match ($json['type'])` and Rust gets a typed
//!   enum via serde.
//! * Unknown message types are **not** an error – [`ProtocolMessage::Unknown`]
//!   absorbs them. That lets the server ship new messages without breaking
//!   older clients.
//! * The server is authoritative: `Input` is the only **send** message that
//!   carries gameplay state; every truth comes from `snapshot` / `event`.
//!
//! # Message types
//!
//! | direction | type             | purpose                                       |
//! |-----------|------------------|-----------------------------------------------|
//! | → server  | `hello`          | handshake + client identity                   |
//! | → server  | `input`          | one input sample (the AGENTS.md example)      |
//! | → server  | `ping`           | latency measurement                           |
//! | ← client  | `welcome`        | handshake ack: player id, tick, tick rate     |
//! | ← client  | `snapshot`       | authoritative world state                     |
//! | ← client  | `event`          | discrete gameplay events (hits, round start…) |
//! | ← client  | `round_state`    | lobby / countdown / running / finished        |
//! | ← client  | `pong`           | latency measurement reply                     |
//! | ← client  | `error`          | recoverable or fatal protocol error           |
//!
//! All snapshots/events are *mirrored into the client state* by
//! `net::client::GameClient`; gameplay modules never deserialize JSON.

pub mod codec;
pub mod message;
pub mod validation;

pub use codec::{DecodeFailure, parse_frame};
pub use message::*;
pub use validation::Validate;

/// Current protocol version. Bumped on any breaking wire change; the server
/// should reject mismatched clients (and the client logs a warning on `welcome`).
pub const PROTOCOL_VERSION: u32 = 1;
