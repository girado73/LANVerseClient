//! Input sampling and the local input history.
//!
//! # Responsibility split
//!
//! * [`InputSource`] is a *trait*: anything that can produce an [`InputSample`]
//!   for the current tick. The raylib keyboard implementation lives in
//!   `platform::desktop`, a scripted source exists for tests/headless mode.
//! * [`InputBuffer`] stores samples by sequence so the server ack
//!   (`snapshot.acked_input_sequence`) can replay everything newer.
//! * The client only ever communicates *intent*. Nothing here applies movement.
//!
//! Jump/dash are **edge-triggered**: holding the key must not repeat the action
//! every tick, so the sampler consumes the pressed-set into one-shot actions.

pub mod buffer;
pub mod source;

use serde::{Deserialize, Serialize};

use crate::core::{Tick, Vec2};
use crate::protocol::message::InputMessage;

pub use buffer::InputBuffer;
pub use source::{InputSample, InputSource, ScriptedInputSource};

/// One-shot intents the server may act on.
///
/// Serialized as lowercase strings (`"jump"`) so the server reads them
/// from a simple array without positional coupling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputActions {
    Jump,
    Dash,
    UseItem,
    Interact,
    Emote,
}

/// Stateful input pipeline: samples raw movement + edge-triggered actions and
/// stitches them into a protocol message with our local tick/sequence.
#[derive(Debug)]
pub struct InputPipeline {
    sequence: u64,
    pending_actions: Vec<InputActions>,
    last_direction: Vec2,
    deadzone: f32,
}

impl InputPipeline {
    pub fn new(deadzone: f32) -> Self {
        Self {
            sequence: 0,
            pending_actions: Vec::new(),
            last_direction: Vec2::RIGHT,
            deadzone,
        }
    }

    /// Last non-zero movement direction (for aiming/UI), never zero.
    pub fn last_direction(&self) -> Vec2 {
        self.last_direction
    }

    /// Applies a deadzone and clamps the vector to unit length.
    pub fn sanitize_movement(&self, raw: Vec2) -> Vec2 {
        if raw.length() < self.deadzone {
            Vec2::ZERO
        } else {
            raw.clamped_length(1.0)
        }
    }

    /// Queues an edge-triggered action for the next called [`InputPipeline::sample`].
    pub fn push_action(&mut self, action: InputActions) {
        if !self.pending_actions.contains(&action) {
            self.pending_actions.push(action);
        }
    }

    /// Builds the next [`InputMessage`] for `tick`.
    ///
    /// One-shot actions are flushed into this sample, so each press is sent
    /// exactly once (built for UDP-unreliable delivery, the client does not
    /// resend actions – the server is expected to ack or discard them).
    pub fn sample(&mut self, tick: Tick, raw_movement: Vec2) -> InputMessage {
        let movement = self.sanitize_movement(raw_movement);
        if !movement.is_zero() {
            self.last_direction = movement.normalized();
        }

        self.sequence += 1;
        InputMessage {
            client_tick: tick.0,
            sequence: self.sequence,
            movement,
            actions: std::mem::take(&mut self.pending_actions),
        }
    }

    pub fn last_sequence(&self) -> u64 {
        self.sequence
    }

    /// Moves the counter forward so the next sample is `sequence + 1`.
    ///
    /// Used after a reconnect: the server ignores any input whose sequence is
    /// not greater than the highest one it already applied from this player.
    /// The counter never moves backwards.
    pub fn ensure_sequence_after(&mut self, sequence: u64) {
        self.sequence = self.sequence.max(sequence);
    }

    /// Drops queued actions without emitting them (used when a press becomes
    /// meaningless, e.g. while the connection is down).
    pub fn clear_pending_actions(&mut self) {
        self.pending_actions.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deadzone_zeroes_small_inputs() {
        let mut pipeline = InputPipeline::new(0.2);
        let msg = pipeline.sample(Tick(1), Vec2::new(0.05, 0.0));
        assert_eq!(msg.movement, Vec2::ZERO);
    }

    #[test]
    fn movement_is_clamped_to_unit_length() {
        let mut pipeline = InputPipeline::new(0.0);
        let msg = pipeline.sample(Tick(1), Vec2::new(5.0, 5.0));
        assert!((msg.movement.length() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn sequence_is_monotonic_and_actions_are_one_shot() {
        let mut pipeline = InputPipeline::new(0.0);
        pipeline.push_action(InputActions::Jump);
        pipeline.push_action(InputActions::Jump); // duplicate is ignored

        let first = pipeline.sample(Tick(1), Vec2::RIGHT);
        assert_eq!(first.actions, vec![InputActions::Jump]);
        assert_eq!(first.sequence, 1);

        let second = pipeline.sample(Tick(2), Vec2::RIGHT);
        assert!(second.actions.is_empty());
        assert_eq!(second.sequence, 2);
    }

    #[test]
    fn sequence_can_be_moved_forward_after_reconnect() {
        let mut pipeline = InputPipeline::new(0.0);
        pipeline.sample(Tick(1), Vec2::ZERO);
        pipeline.ensure_sequence_after(500);
        assert_eq!(
            pipeline.sample(Tick(2), Vec2::ZERO).sequence,
            501,
            "sequences must stay strictly increasing for the server"
        );
        // Never moves backwards.
        pipeline.ensure_sequence_after(3);
        assert_eq!(pipeline.sample(Tick(3), Vec2::ZERO).sequence, 502);
    }

    #[test]
    fn direction_remembers_last_non_zero_movement() {
        let mut pipeline = InputPipeline::new(0.0);
        pipeline.sample(Tick(1), Vec2::LEFT);
        pipeline.sample(Tick(2), Vec2::ZERO);
        assert_eq!(pipeline.last_direction(), Vec2::LEFT);
    }
}
