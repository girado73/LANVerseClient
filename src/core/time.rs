//! Simulation time helpers.
//!
//! The client runs its input/prediction loop on a **fixed tick rate**
//! (see `simulation::tick_rate`) independent of the render framerate. This
//! module provides the [`Tick`] counter and the conversion helpers shared by
//! input, prediction and interpolation.

use std::fmt;
use std::ops::{Add, AddAssign, Sub};

use serde::{Deserialize, Serialize};

/// A discrete simulation step at a fixed rate. Monotonic, wraps never in practice.
#[derive(
    Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct Tick(pub u64);

impl Tick {
    pub const ZERO: Self = Self(0);

    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Signed distance `other - self` in ticks.
    pub fn signed_delta_to(self, other: Self) -> i64 {
        other.0 as i64 - self.0 as i64
    }
}

impl Add<u64> for Tick {
    type Output = Self;
    fn add(self, rhs: u64) -> Self {
        Self(self.0 + rhs)
    }
}

impl AddAssign<u64> for Tick {
    fn add_assign(&mut self, rhs: u64) {
        self.0 += rhs;
    }
}

impl Sub<Tick> for Tick {
    type Output = u64;
    fn sub(self, rhs: Self) -> u64 {
        self.0.saturating_sub(rhs.0)
    }
}

impl fmt::Display for Tick {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "t{}", self.0)
    }
}

/// Convenience helpers for converting real time into ticks.
pub trait DurationExt {
    /// Number of whole ticks that fit into this duration at the given rate.
    fn ticks_at(self, ticks_per_second: u32) -> Tick;
}

impl DurationExt for std::time::Duration {
    fn ticks_at(self, ticks_per_second: u32) -> Tick {
        Tick((self.as_secs_f64() * f64::from(ticks_per_second)) as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn tick_delta_can_be_negative() {
        assert_eq!(Tick(10).signed_delta_to(Tick(4)), -6);
        assert_eq!(Tick(4).signed_delta_to(Tick(10)), 6);
    }

    #[test]
    fn duration_to_ticks() {
        assert_eq!(Duration::from_millis(500).ticks_at(60), Tick(30));
    }
}
