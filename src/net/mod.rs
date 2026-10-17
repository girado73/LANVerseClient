//! Networking layer.
//!
//! # Architecture
//!
//! ```text
//!  ┌─────────────────────────── game loop (main thread) ────────────────────────────┐
//!  │                                                                                 │
//!  │  InputPipeline ──send──► NetClientHandle ──mpsc──► [net thread / tokio task]    │
//!  │        ▲                       │                    Transport::connect()        │
//!  │        │                       │                    ├─ WebSocketTransport ──► Laravel (Reverb/Echo)
//!  │        │                       │                    └─ MockTransport      (tests)
//!  │  prediction ◄──GameEvent── GameClient ◄──mpsc── [net thread / tokio task]       │
//!  │  visuals/UI    (drained)        │                                               │
//!  │                                 └── NetEvents snapshot/status                    │
//!  └─────────────────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! The `net` module never touches raylib – it is pure async I/O. All game
//! logic consumes [`NetEvent`]s drained once per frame by
//! [`client::GameClient`].
//!
//! * [`transport`] – the byte-level link (WebSocket, in-memory mock).
//! * [`connection`] – status/state machine + reconnect policy, transport agnostic.
//! * [`session`] – the async driver connecting transport + handshake + heartbeat.
//! * [`client`] – the game-facing API (`GameClient`, `apply_snapshot`, …).
//! * [`events`] – the typed event stream shared by the game loop.
//! * [`latency`] – RTT estimation used for prediction/reconciliation.

pub mod client;
pub mod connection;
pub mod events;
pub mod latency;
pub mod mock;
pub mod session;
pub mod transport;

pub use client::GameClient;
pub use connection::{ConnectionStatus, NetClientHandle};
pub use events::NetEvent;
pub use mock::MockTransport;
pub use transport::{Framing, MessageTransport, TransportError, WsTransport};
