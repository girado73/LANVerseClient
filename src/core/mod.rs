//! Core building blocks shared by every subsystem of the client.
//!
//! Nothing in here knows about rendering, networking or gameplay – it is the
//! vocabulary (time, math, identifiers, ticks, sequences) that the other
//! modules speak. Keeping these types in one place avoids circular module
//! dependencies as the client grows.

pub mod clock;
pub mod ids;
pub mod math;
pub mod time;

pub use clock::FixedClock;
pub use ids::{MatchId, PlayerId, RoundId, SessionId};
pub use math::Vec2;
pub use time::{DurationExt, Tick};
