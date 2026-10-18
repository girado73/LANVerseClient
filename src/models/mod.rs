//! In-memory representation of the game world.
//!
//! Everything in here is a **mirror of server state** (or purely visual
//! decoration derived from it). No type in this module may be mutated to
//! "decide" a gameplay outcome – it is written by `net::client` when snapshots
//! arrive and read by rendering / prediction.
//!
//! Submodules:
//! * [`world`] – authoritative-ish entities (players, round info).
//! * [`game_state`] – the aggregate client-side view (connection, phase, players).
//! * [`visual`] – purely local animation state (hit flashes, walk cycles).

pub mod game_state;
pub mod visual;
pub mod world;

pub use game_state::{GameState, WorldBounds};
pub use visual::{AnimationClock, PlayerVisuals, VisualState};
pub use world::{MarkerState, PlayerState, RoundInfo};
