//! Application shell: the game loop.
//!
//! [`ClientApp`] is the only place that owns *all* subsystems and decides the
//! order of operations per frame:
//!
//! ```text
//! every frame:
//!   1. platform: poll window/frame time
//!   2. net:      drain NetEvents → GameState (authoritative)
//!   3. fixed-tick loop (0..n):
//!        a. sample input (keyboard or scripted)
//!        b. sample → InputMessage, store in buffer, send to server
//!        c. prediction.predict() → local_override (render only)
//!   4. visuals:  advance local animations, drain FxEvents → audio
//!   5. render:   world → internal texture → upscale to window
//! ```
//!
//! The three layers are strictly ordered: **network truth is applied before
//! prediction, prediction before rendering.** Nothing in the tick loop can
//! overwrite authoritative fields.

pub mod loop_runner;
pub mod runtime;

pub use loop_runner::{ClientApp, ExitReason, RunMode};
pub use runtime::{build_tokio_runtime, init_tracing};
