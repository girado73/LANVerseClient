//! Client-side prediction & reconciliation.
//!
//! # Why
//!
//! Without prediction the local player would visually lag by one RTT. We
//! therefore move our own sprite immediately using a *predicted* velocity.
//!
//! # How (and why it stays honest)
//!
//! ```text
//! snapshot (server truth at tick T, acked sequence S)
//!     │
//!     ├─► reconcile(): start from server position at T
//!     └─► replay every unacked input with sequence > S
//!              │
//!              └─► predicted position for rendering only
//! ```
//!
//! 1. The authoritative position is copied from the snapshot.
//! 2. Inputs the server has acknowledged are dropped from the replay buffer.
//! 3. Remaining inputs are integrated with the *client's estimate* of the
//!    movement speed. If that estimate is wrong, the correction from the next
//!    snapshot hides it within a few ticks.
//!
//! The prediction result never leaves this module except as a render position,
//! and [`Prediction::correction`] exposes how far off we were so a renderer
//! can smooth the snap back instead of teleporting.

use crate::core::{PlayerId, Tick, Vec2};
use crate::input::buffer::InputBuffer;
use crate::protocol::message::Snapshot;

/// Estimated movement parameters.
///
/// These are *guesses* used for visual prediction. They must never be used to
/// validate or override server data – if the server says otherwise, it wins.
/// A wrong speed is visible as rubber-banding, so the default mirrors the
/// Server default (220 units/s) until
/// the server announces `welcome.move_speed`.
#[derive(Debug, Clone, Copy)]
pub struct MovementModel {
    /// World units per second at full input.
    pub speed: f32,
    /// Fraction of speed kept while airborne / dashing, reserved for future modes.
    pub air_control: f32,
}

impl Default for MovementModel {
    fn default() -> Self {
        Self {
            speed: 220.0,
            air_control: 1.0,
        }
    }
}

impl MovementModel {
    /// Prediction speed announced by the server via `welcome.move_speed`.
    ///
    /// Lua modes may move players faster or slower (the manifest sets
    /// `player.move_speed`); without this the local sprite rubber-bands against
    /// the authoritative position.
    pub fn with_speed(speed: Option<f32>) -> Self {
        match speed {
            Some(speed) if speed.is_finite() && speed > 0.0 => Self {
                speed,
                ..Self::default()
            },
            _ => Self::default(),
        }
    }

    /// Displacement for one tick of `input.movement`.
    pub fn displacement(&self, movement: Vec2, tick_rate: u32) -> Vec2 {
        movement * (self.speed / tick_rate.max(1) as f32)
    }
}

/// Result of prediction for one tick.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Prediction {
    pub tick: Tick,
    /// Where we believe our player is (render only).
    pub position: Vec2,
    /// Vector from server position to predicted position, for smoothing.
    pub correction: Vec2,
    /// Number of unacked inputs replayed.
    pub replayed_inputs: usize,
    /// Whether we had authoritative data to start from.
    pub has_authority: bool,
}

impl Prediction {
    pub fn empty(tick: Tick) -> Self {
        Self {
            tick,
            position: Vec2::ZERO,
            correction: Vec2::ZERO,
            replayed_inputs: 0,
            has_authority: false,
        }
    }
}

/// Holds the last authoritative snapshot for our player and computes the
/// predicted position.
#[derive(Debug)]
pub struct Predictor {
    model: MovementModel,
    local_player: Option<PlayerId>,
    /// Last authoritative position + tick.
    authority: Option<(Tick, Vec2)>,
}

impl Predictor {
    pub fn new(model: MovementModel) -> Self {
        Self {
            model,
            local_player: None,
            authority: None,
        }
    }
    pub fn model(&self) -> &MovementModel {
        &self.model
    }

    pub fn set_local_player(&mut self, id: Option<PlayerId>) {
        if self.local_player != id {
            self.authority = None;
        }
        self.local_player = id;
    }

    /// Applies an authoritative snapshot: stores our player's position and
    /// acknowledges inputs.
    pub fn on_snapshot(&mut self, snapshot: &Snapshot, inputs: &mut InputBuffer) -> bool {
        if let Some(acked) = snapshot.acked_input_sequence {
            inputs.acknowledge_through(acked);
        }

        let Some(local_id) = &self.local_player else {
            return false;
        };
        let Some(player) = snapshot.players.iter().find(|p| &p.id == local_id) else {
            // We are not in the snapshot (not spawned yet or dead) – do not
            // pretend to know where we are.
            self.authority = None;
            return false;
        };

        if let Some((tick, _)) = self.authority
            && tick.0 > snapshot.tick
        {
            return false;
        }

        self.authority = Some((Tick(snapshot.tick), player.position));
        true
    }

    /// Computes the predicted position for rendering.
    ///
    /// * `current_tick` – our local tick counter.
    /// * `inputs` – the unacked inputs (already pruned by `on_snapshot`).
    pub fn predict(
        &mut self,
        current_tick: Tick,
        inputs: &InputBuffer,
        tick_rate: u32,
    ) -> Prediction {
        let Some((_, mut position)) = self.authority else {
            return Prediction::empty(current_tick);
        };
        let authority_position = position;

        // `pending()` already contains exactly the inputs the server has *not*
        // acknowledged (`on_snapshot` prunes by sequence), so replaying all of
        // them is the correct reconciliation. Comparing `client_tick` with the
        // server tick was wrong: the two clocks start at different values
        // (a late joiner would never replay anything).
        let mut replayed = 0;
        for input in inputs.pending() {
            position += self.model.displacement(input.movement, tick_rate);
            replayed += 1;
        }

        Prediction {
            tick: current_tick,
            position,
            // Distance between prediction and server base – diagnostics only.
            // The renderer must use `position` as-is: adding the correction on
            // top double-counted the replayed movement and made the sprite
            // jitter by one input per ack.
            correction: position - authority_position,
            replayed_inputs: replayed,
            has_authority: true,
        }
    }

    pub fn reset(&mut self) {
        self.authority = None;
    }

    /// Replaces the estimated movement speed with the server-announced value.
    ///
    /// This is *still* only used for visual prediction; the server position
    /// overwrites everything on the next snapshot. `None`/invalid values keep
    /// the current estimate.
    pub fn set_speed(&mut self, speed: Option<f32>) {
        if let Some(speed) = speed
            && speed.is_finite()
            && speed > 0.0
        {
            self.model.speed = speed;
        }
    }
}
/// Utility: build a snapshot with our player for tests.
#[cfg(test)]
pub(crate) fn snapshot_with_player(
    tick: u64,
    player: PlayerId,
    position: Vec2,
    acked: Option<u64>,
) -> Snapshot {
    use crate::models::world::PlayerState;
    Snapshot {
        tick,
        acked_input_sequence: acked,
        players: vec![PlayerState {
            id: player,
            position,
            ..Default::default()
        }],
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Vec2;
    use crate::protocol::message::InputMessage;

    fn input(sequence: u64, tick: u64, movement: Vec2) -> InputMessage {
        InputMessage {
            client_tick: tick,
            sequence,
            movement,
            actions: vec![],
        }
    }

    fn setup() -> (Predictor, InputBuffer, PlayerId) {
        let id = PlayerId::from("me");
        let mut predictor = Predictor::new(MovementModel {
            speed: 60.0,
            air_control: 1.0,
        });
        predictor.set_local_player(Some(id.clone()));
        (predictor, InputBuffer::new(64), id)
    }

    #[test]
    fn without_authority_there_is_no_prediction() {
        let (mut predictor, inputs, _id) = setup();
        let prediction = predictor.predict(Tick(5), &inputs, 60);
        assert!(!prediction.has_authority);
        assert_eq!(prediction.position, Vec2::ZERO);
    }

    #[test]
    fn replays_only_unacked_inputs_on_top_of_server_position() {
        let (mut predictor, mut inputs, id) = setup();
        // Server says: you are at (10,0) and I have processed input #2.
        let snapshot = snapshot_with_player(100, id, Vec2::new(10.0, 0.0), Some(2));
        predictor.on_snapshot(&snapshot, &mut inputs);
        assert!(inputs.is_empty()); // nothing was buffered

        // Now two newer inputs are in flight: ticks 101, 102 moving right.
        inputs.push(input(3, 101, Vec2::RIGHT));
        inputs.push(input(4, 102, Vec2::RIGHT));

        let prediction = predictor.predict(Tick(102), &inputs, 60);
        assert!(prediction.has_authority);
        assert_eq!(prediction.replayed_inputs, 2);
        // speed 60 / 60 Hz = 1 unit per tick, two ticks -> +2
        assert!((prediction.position.x - 12.0).abs() < 1e-4);
        assert!((prediction.correction.x - 2.0).abs() < 1e-4);
    }

    #[test]
    fn ack_prunes_buffer_and_correction_shrinks() {
        let (mut predictor, mut inputs, id) = setup();
        for sequence in 1..=5 {
            inputs.push(input(sequence, 100 + sequence, Vec2::RIGHT));
        }
        let snapshot = snapshot_with_player(104, id.clone(), Vec2::new(4.0, 0.0), Some(4));
        predictor.on_snapshot(&snapshot, &mut inputs);

        let remaining: Vec<_> = inputs.pending().map(|i| i.sequence).collect();
        assert_eq!(remaining, [5]);

        let prediction = predictor.predict(Tick(105), &inputs, 60);
        assert_eq!(prediction.replayed_inputs, 1);
        assert!((prediction.position.x - 5.0).abs() < 1e-4);
    }

    #[test]
    fn negative_correction_is_reported_after_wrong_guess() {
        let (mut predictor, mut inputs, id) = setup();
        inputs.push(input(1, 101, Vec2::RIGHT));
        // Server actually did not move us (wall). Correction must be negative.
        let snapshot = snapshot_with_player(101, id, Vec2::ZERO, Some(1));
        predictor.on_snapshot(&snapshot, &mut inputs);
        let prediction = predictor.predict(Tick(101), &inputs, 60);
        // Acked input is not replayed, so we sit exactly on the server position.
        assert_eq!(prediction.correction, Vec2::ZERO);
    }

    #[test]
    fn stale_snapshot_is_ignored() {
        let (mut predictor, mut inputs, id) = setup();
        let fresh = snapshot_with_player(200, id.clone(), Vec2::new(50.0, 0.0), None);
        predictor.on_snapshot(&fresh, &mut inputs);
        let stale = snapshot_with_player(100, id, Vec2::new(0.0, 0.0), None);
        predictor.on_snapshot(&stale, &mut inputs);
        assert_eq!(predictor.authority.map(|a| a.1), Some(Vec2::new(50.0, 0.0)));
    }
}
