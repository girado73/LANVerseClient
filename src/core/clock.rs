//! Fixed-timestep accumulator.
//!
//! Rendering runs at whatever framerate the monitor offers, but input,
//! prediction and snapshot ingestion must happen in discrete [`Tick`]s.
//! [`FixedClock`] converts variable frame time into a bounded number of fixed
//! steps so a long stall (window drag, debugger pause) cannot cause a spiral
//! of catch-up ticks.

use super::time::Tick;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct FixedClock {
    tick_rate: u32,
    tick_duration: Duration,
    accumulator: Duration,
    current_tick: Tick,
    max_steps_per_frame: u32,
}

impl FixedClock {
    pub fn new(tick_rate: u32, start_tick: Tick) -> Self {
        assert!(tick_rate > 0, "tick rate must be positive");
        Self {
            tick_rate,
            // Integer nanoseconds: 1/60 s as f64 truncates to 16_666_666 ns,
            // which would make the clock drift fast (one tick every few hundred
            // frames). Rounding keeps the accumulator exact for common rates.
            tick_duration: Duration::from_nanos(
                (1_000_000_000u64 + tick_rate as u64 / 2) / tick_rate as u64,
            ),
            accumulator: Duration::ZERO,
            current_tick: start_tick,
            max_steps_per_frame: 8,
        }
    }

    /// Maximum number of ticks advanced by a single [`FixedClock::advance`] call.
    pub fn with_max_steps(mut self, max_steps: u32) -> Self {
        self.max_steps_per_frame = max_steps.max(1);
        self
    }

    pub fn tick_rate(&self) -> u32 {
        self.tick_rate
    }

    pub fn tick_duration(&self) -> Duration {
        self.tick_duration
    }

    pub fn current_tick(&self) -> Tick {
        self.current_tick
    }

    /// Feeds one rendered frame into the accumulator and returns the number of
    /// fixed ticks that should be simulated this frame (0..=max_steps).
    pub fn advance(&mut self, frame_delta: Duration) -> u32 {
        // Guard against absurd frame deltas (e.g. the process was suspended).
        let capped = frame_delta.min(self.tick_duration * self.max_steps_per_frame);
        self.accumulator += capped;

        let mut steps = 0;
        while self.accumulator >= self.tick_duration && steps < self.max_steps_per_frame {
            self.accumulator -= self.tick_duration;
            self.current_tick += 1;
            steps += 1;
        }

        // If we hit the cap, drop the remaining backlog instead of accumulating
        // unbounded lag. The server state will re-sync us anyway.
        if steps == self.max_steps_per_frame && self.accumulator >= self.tick_duration {
            self.accumulator = Duration::ZERO;
        }

        steps
    }

    /// How far we are between the last simulated tick and the next one, in the
    /// range `0.0..1.0`. Interpolation uses this as its blend factor.
    pub fn alpha(&self) -> f32 {
        (self.accumulator.as_secs_f64() / self.tick_duration.as_secs_f64()) as f32
    }

    /// Re-bases the clock, e.g. after a server resync sent us its tick counter.
    pub fn set_current_tick(&mut self, tick: Tick) {
        self.current_tick = tick;
        self.accumulator = Duration::ZERO;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accumulates_partial_frames_into_whole_ticks() {
        let mut clock = FixedClock::new(60, Tick::ZERO);
        // 30 fps → exactly two 60 Hz ticks per frame at 33.333… ms.
        let frame = Duration::from_nanos(1_000_000_000 / 30);

        clock.advance(frame);
        assert_eq!(clock.current_tick(), Tick(1));
        // The reminder carries over, so the second 16.67 ms boundary is hit
        // during the next frame.
        clock.advance(frame);
        assert_eq!(clock.current_tick(), Tick(3));
    }

    #[test]
    fn alpha_reports_progress_within_the_tick() {
        let mut clock = FixedClock::new(60, Tick::ZERO);
        // Half a tick (half of 16.67 ms) → step 0, alpha ≈ 0.5.
        let steps = clock.advance(Duration::from_micros(8_333));
        assert_eq!(steps, 0);
        assert!(
            (clock.alpha() - 0.5).abs() < 0.01,
            "alpha {}",
            clock.alpha()
        );
    }

    #[test]
    fn caps_catch_up_steps() {
        let mut clock = FixedClock::new(60, Tick::ZERO).with_max_steps(4);
        let steps = clock.advance(Duration::from_secs(5));
        assert_eq!(steps, 4);
        assert_eq!(clock.current_tick(), Tick(4));
        // backlog is dropped, next normal frame only advances normally
        assert!(clock.advance(Duration::from_millis(16)) <= 1);
    }
}
