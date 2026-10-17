//! Simulation constants and helpers shared by input, prediction and render.
//!
//! The client *must* use the tick rate the server announced in its `welcome`.
//! [`TickRate`] wraps that value together with conversion helpers so no module
//! hardcodes 60 Hz.

use std::time::Duration;

use crate::core::Tick;

/// Default simulation rate used before the server says otherwise.
pub const DEFAULT_TICK_RATE: u32 = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TickRate(u32);

impl TickRate {
    pub const DEFAULT: Self = Self(DEFAULT_TICK_RATE);

    pub fn new(ticks_per_second: u32) -> Self {
        Self(ticks_per_second.max(1))
    }

    pub fn hz(&self) -> u32 {
        self.0
    }

    pub fn tick_duration(&self) -> Duration {
        Duration::from_secs_f64(1.0 / f64::from(self.0))
    }

    pub fn ms_to_ticks(&self, milliseconds: f32) -> f32 {
        milliseconds / 1000.0 * self.0 as f32
    }

    pub fn ticks_to_seconds(&self, ticks: f32) -> f32 {
        ticks / self.0 as f32
    }

    pub fn tick_at(&self, elapsed: Duration) -> Tick {
        Tick((elapsed.as_secs_f64() * f64::from(self.0)) as u64)
    }
}

impl Default for TickRate {
    fn default() -> Self {
        Self::DEFAULT
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversions_are_consistent() {
        let rate = TickRate::new(60);
        assert!((rate.ms_to_ticks(100.0) - 6.0).abs() < 1e-4);
        assert!((rate.ticks_to_seconds(60.0) - 1.0).abs() < 1e-4);
        assert_eq!(rate.tick_at(Duration::from_secs(2)), Tick(120));
    }

    #[test]
    fn zero_hz_is_clamped() {
        assert_eq!(TickRate::new(0).hz(), 1);
    }
}
