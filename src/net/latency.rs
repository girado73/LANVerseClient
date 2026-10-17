//! Round-trip latency estimation and clock offset estimation.
//!
//! Prediction needs to know how "old" the authoritative state is, and
//! interpolation needs to add the render delay of roughly half the RTT.
//!
//! The estimator is intentionally simple (exponential moving average) – LAN
//! latency is stable, and a Kalman filter would be overkill.

use std::time::{SystemTime, UNIX_EPOCH};

const SMOOTHING: f32 = 0.2;

#[derive(Debug, Clone)]
pub struct LatencyEstimator {
    smoothed_rtt_ms: Option<f32>,
    samples: u32,
    /// Half of the smoothed RTT – used as interpolation delay.
    interpolation_delay_ms: f32,
}

impl Default for LatencyEstimator {
    fn default() -> Self {
        Self {
            smoothed_rtt_ms: None,
            samples: 0,
            interpolation_delay_ms: 0.0,
        }
    }
}

impl LatencyEstimator {
    /// Milliseconds since the Unix epoch (monotonic enough for RTT deltas and
    /// readable on the server side for debugging).
    pub fn now_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }

    /// Feeds the echoed client timestamp of a pong; returns the new smoothed RTT.
    pub fn on_pong(&mut self, client_time_ms: u64) -> Option<f32> {
        let now = Self::now_ms();
        // Clock could have moved backwards (NTP) – ignore the sample.
        let rtt = now.saturating_sub(client_time_ms) as f32;
        self.samples += 1;

        self.smoothed_rtt_ms = Some(match self.smoothed_rtt_ms {
            None => rtt,
            Some(previous) => previous * (1.0 - SMOOTHING) + rtt * SMOOTHING,
        });
        self.interpolation_delay_ms = self.smoothed_rtt_ms.unwrap_or(0.0) * 0.5;
        self.smoothed_rtt_ms
    }

    pub fn rtt_ms(&self) -> Option<f32> {
        self.smoothed_rtt_ms
    }

    pub fn samples(&self) -> u32 {
        self.samples
    }

    /// Delay (ms) to add to the render clock so we always render between two
    /// received snapshots instead of extrapolating.
    pub fn interpolation_delay_ms(&self) -> f32 {
        self.interpolation_delay_ms
    }

    /// How many ticks of prediction compensate the current latency.
    pub fn latency_ticks(&self, tick_rate: u32) -> u64 {
        let rtt = self.smoothed_rtt_ms.unwrap_or(0.0);
        (rtt / 1000.0 * f32::from(tick_rate as u16)).round() as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_sample_sets_rtt_and_halves_it_for_interpolation() {
        let mut estimator = LatencyEstimator::default();
        let rtt = estimator.on_pong(LatencyEstimator::now_ms()).unwrap();
        assert!(rtt < 50.0, "loopback should be fast, got {rtt}");
        assert_eq!(estimator.samples(), 1);
        assert!((estimator.interpolation_delay_ms() - rtt * 0.5).abs() < 1e-3);
    }

    #[test]
    fn latency_ticks_rounds_to_nearest() {
        let mut estimator = LatencyEstimator::default();
        estimator.on_pong(LatencyEstimator::now_ms() - 100);
        // ~100ms RTT at 60 ticks/s ≈ 6 ticks
        assert!((5..=7).contains(&estimator.latency_ticks(60)));
    }

    #[test]
    fn pong_from_the_future_is_ignored_as_zero() {
        let mut estimator = LatencyEstimator::default();
        let rtt = estimator
            .on_pong(LatencyEstimator::now_ms() + 5_000)
            .unwrap();
        assert_eq!(rtt, 0.0);
    }
}
