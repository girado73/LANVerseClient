//! Abstractions over "where does input come from".
//!
//! The game loop never touches the keyboard directly; it asks an
//! [`InputSource`] for the movement vector and flushes edge-triggered actions
//! into the [`InputPipeline`](crate::input::InputPipeline). That keeps the
//! simulation testable without a window and lets us add gamepad support by
//! implementing a composite source later.

use crate::core::Vec2;
use crate::input::InputActions;

/// Raw, unsanitized input state for one fixed tick.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct InputSample {
    /// Raw movement intent (can exceed length 1 for digital input, is sanitized later).
    pub movement: Vec2,
    /// Actions triggered *this* tick (edge-triggered).
    pub actions: Vec<InputActions>,
}

impl InputSample {
    pub fn is_idle(&self) -> bool {
        self.movement.is_zero() && self.actions.is_empty()
    }
}

/// Anything that can be polled once per fixed tick.
pub trait InputSource: Send {
    /// Returns the input for the current tick. Called at the fixed tick rate.
    fn poll(&mut self) -> InputSample;
}

/// Deterministic input generator for tests and `--headless` smoke runs.
///
/// Produces a slow circular movement with a jump every 60 ticks.
#[derive(Debug, Clone, Default)]
pub struct ScriptedInputSource {
    tick: u64,
}

impl ScriptedInputSource {
    pub fn new() -> Self {
        Self::default()
    }
}

impl InputSource for ScriptedInputSource {
    fn poll(&mut self) -> InputSample {
        self.tick += 1;
        let angle = self.tick as f32 * 0.05;
        let mut actions = Vec::new();
        if self.tick.is_multiple_of(60) {
            actions.push(InputActions::Jump);
        }
        InputSample {
            movement: Vec2::new(angle.cos(), angle.sin()),
            actions,
        }
    }
}

/// Failsafe source used when no window/input backend exists.
#[derive(Debug, Clone, Copy, Default)]
pub struct NullInputSource;

impl InputSource for NullInputSource {
    fn poll(&mut self) -> InputSample {
        InputSample::default()
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripted_source_emits_regular_jumps() {
        let mut source = ScriptedInputSource::new();
        let jumps: Vec<u64> = (1..=120)
            .filter(|_| source.poll().actions.contains(&InputActions::Jump))
            .collect();
        // ticks 1..=120 polled -> jump at poll indices 60 and 120
        assert_eq!(jumps.len(), 2);
    }
}
