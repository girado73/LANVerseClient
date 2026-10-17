//! Local simulation: tick rate, prediction, reconciliation, fixed step.
//!
//! **Nothing in this module decides gameplay outcomes.** Prediction only
//! produces a *display position* for our own player so input feels immediate;
//! as soon as a snapshot arrives it is corrected back to server truth and the
//! still-unacked inputs are replayed on top.

pub mod prediction;
pub mod tick_rate;

pub use prediction::{Prediction, Predictor};
pub use tick_rate::{DEFAULT_TICK_RATE, TickRate};
