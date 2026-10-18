//! Validation of parsed messages.
//!
//! The server is the source of truth, but its data still has to be
//! *well-formed* before it may overwrite local state. A snapshot with `NaN`
//! positions, duplicate player ids or an implausible tick would poison
//! interpolation and prediction for the rest of the session, so such frames
//! are dropped (and logged) instead of being applied.

use crate::models::world::PlayerState;
use crate::protocol::message::{InputMessage, Snapshot, Welcome};

/// Upper bound for a plausible simulation rate.
const MAX_TICK_RATE: u32 = 240;
/// Max number of players we are willing to track from one snapshot.
const MAX_PLAYERS: usize = 512;

/// Something that can be checked before it is trusted.
pub trait Validate {
    fn validate(&self) -> Result<(), ValidationError>;
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ValidationError {
    #[error("movement vector is non-finite")]
    NonFiniteMovement,
    #[error("movement vector exceeds length 1 ({0})")]
    MovementTooLong(f32),
    #[error("too many actions in one input ({0})")]
    TooManyActions(usize),
    #[error("unsupported protocol version {found} (client speaks {expected})")]
    UnsupportedProtocolVersion { found: u32, expected: u32 },
    #[error("tick rate {0} is out of bounds")]
    InvalidTickRate(u32),
    #[error("player {0} at index {1} is invalid: {2}")]
    InvalidPlayer(String, usize, String),
    #[error("duplicate player id `{0}` in snapshot")]
    DuplicatePlayer(String),
    #[error("snapshot contains too many players ({0})")]
    TooManyPlayers(usize),
    #[error("world size {0}x{1} is implausible")]
    InvalidWorld(f32, f32),
    #[error("movement speed {0} is implausible")]
    InvalidMoveSpeed(f32),
}

/// Upper bound for a plausible world dimension (world units / pixels).
const MAX_WORLD_DIMENSION: f32 = 1_000_000.0;
/// Upper bound for a plausible player speed in world units per second.
const MAX_MOVE_SPEED: f32 = 10_000.0;

impl Validate for InputMessage {
    fn validate(&self) -> Result<(), ValidationError> {
        if !self.movement.x.is_finite() || !self.movement.y.is_finite() {
            return Err(ValidationError::NonFiniteMovement);
        }
        if self.movement.length() > 1.0 + 1e-3 {
            return Err(ValidationError::MovementTooLong(self.movement.length()));
        }
        if self.actions.len() > 8 {
            return Err(ValidationError::TooManyActions(self.actions.len()));
        }
        Ok(())
    }
}

impl Validate for Welcome {
    fn validate(&self) -> Result<(), ValidationError> {
        if self.protocol_version != crate::protocol::PROTOCOL_VERSION {
            return Err(ValidationError::UnsupportedProtocolVersion {
                found: self.protocol_version,
                expected: crate::protocol::PROTOCOL_VERSION,
            });
        }
        if self.tick_rate == 0 || self.tick_rate > MAX_TICK_RATE {
            return Err(ValidationError::InvalidTickRate(self.tick_rate));
        }
        if let Some(world) = self.world {
            let plausible = |v: f32| v.is_finite() && v > 0.0 && v <= MAX_WORLD_DIMENSION;
            if !plausible(world.width) || !plausible(world.height) {
                return Err(ValidationError::InvalidWorld(world.width, world.height));
            }
        }
        if let Some(speed) = self.move_speed
            && !(speed.is_finite() && speed > 0.0 && speed <= MAX_MOVE_SPEED)
        {
            return Err(ValidationError::InvalidMoveSpeed(speed));
        }
        Ok(())
    }
}

impl Validate for PlayerState {
    fn validate(&self) -> Result<(), ValidationError> {
        use crate::protocol::validation::ValidationError as E;

        let finite = |v: crate::core::Vec2| v.x.is_finite() && v.y.is_finite();
        if !finite(self.position) {
            return Err(E::InvalidPlayer(
                self.id.0.clone(),
                0,
                "non-finite position".to_owned(),
            ));
        }
        if !finite(self.velocity) || !finite(self.facing) {
            return Err(E::InvalidPlayer(
                self.id.0.clone(),
                0,
                "non-finite velocity/facing".to_owned(),
            ));
        }
        if self.max_hp == 0 || self.hp > self.max_hp {
            return Err(E::InvalidPlayer(
                self.id.0.clone(),
                0,
                format!("implausible hp {}/{}", self.hp, self.max_hp),
            ));
        }
        // Guards against a server bug teleporting a player to 1e30 and breaking
        // every camera interpolation afterwards.
        const MAX_ABS_COORD: f32 = 1_000_000.0;
        if self.position.x.abs() > MAX_ABS_COORD || self.position.y.abs() > MAX_ABS_COORD {
            return Err(E::InvalidPlayer(
                self.id.0.clone(),
                0,
                "position out of bounds".to_owned(),
            ));
        }
        Ok(())
    }
}

impl Validate for Snapshot {
    fn validate(&self) -> Result<(), ValidationError> {
        if self.players.len() > MAX_PLAYERS {
            return Err(ValidationError::TooManyPlayers(self.players.len()));
        }

        let mut seen = std::collections::HashSet::with_capacity(self.players.len());
        for (index, player) in self.players.iter().enumerate() {
            if !seen.insert(player.id.as_str()) {
                return Err(ValidationError::DuplicatePlayer(player.id.0.clone()));
            }
            if let Err(reason) = player.validate() {
                return Err(ValidationError::InvalidPlayer(
                    player.id.0.clone(),
                    index,
                    reason.to_string(),
                ));
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Vec2;
    use crate::models::world::PlayerState;
    use crate::protocol::PROTOCOL_VERSION;

    fn player(id: &str, pos: Vec2) -> PlayerState {
        PlayerState {
            id: id.into(),
            name: id.to_owned(),
            position: pos,
            ..Default::default()
        }
    }

    #[test]
    fn rejects_nan_movement() {
        let input = InputMessage {
            client_tick: 0,
            sequence: 0,
            movement: Vec2::new(f32::NAN, 0.0),
            actions: vec![],
        };
        assert_eq!(input.validate(), Err(ValidationError::NonFiniteMovement));
    }

    #[test]
    fn rejects_snapshot_with_duplicate_players() {
        let snap = Snapshot {
            players: vec![player("a", Vec2::ZERO), player("a", Vec2::ONE)],
            ..Default::default()
        };
        assert!(matches!(
            snap.validate(),
            Err(ValidationError::DuplicatePlayer(_))
        ));
    }

    #[test]
    fn rejects_non_finite_player_position() {
        let snap = Snapshot {
            players: vec![player("a", Vec2::new(f32::INFINITY, 0.0))],
            ..Default::default()
        };
        assert!(snap.validate().is_err());
    }

    fn welcome() -> Welcome {
        Welcome {
            protocol_version: PROTOCOL_VERSION,
            player_id: "p".into(),
            session_id: None,
            match_id: None,
            tick_rate: 60,
            server_tick: 0,
            message: None,
            world: None,
            move_speed: None,
        }
    }

    #[test]
    fn rejects_wrong_protocol_version() {
        let welcome = Welcome {
            protocol_version: PROTOCOL_VERSION + 1,
            ..welcome()
        };
        assert!(matches!(
            welcome.validate(),
            Err(ValidationError::UnsupportedProtocolVersion { .. })
        ));
    }

    #[test]
    fn rejects_implausible_world_and_speed() {
        let bad_world = Welcome {
            world: Some(crate::protocol::message::WorldState {
                width: f32::NAN,
                height: 1800.0,
            }),
            ..welcome()
        };
        assert!(matches!(
            bad_world.validate(),
            Err(ValidationError::InvalidWorld(..))
        ));

        let bad_speed = Welcome {
            move_speed: Some(-1.0),
            ..welcome()
        };
        assert!(matches!(
            bad_speed.validate(),
            Err(ValidationError::InvalidMoveSpeed(_))
        ));

        let good = Welcome {
            world: Some(crate::protocol::message::WorldState {
                width: 3200.0,
                height: 1800.0,
            }),
            move_speed: Some(220.0),
            ..welcome()
        };
        good.validate().unwrap();
    }
}
