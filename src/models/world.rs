//! Entity types that mirror server entities.

use serde::{Deserialize, Serialize};

use crate::core::{PlayerId, RoundId, Vec2};

/// One player exactly as the server describes it.
///
/// The client only *reads* these fields for display/prediction; it never
/// writes gameplay-relevant values. Only `name`-independent presentation data
/// is derived from them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlayerState {
    pub id: PlayerId,
    #[serde(default)]
    pub name: String,
    pub position: Vec2,
    /// Reserved: the server currently does not send velocity, but the protocol
    /// tolerates it for interpolation/extrapolation.
    #[serde(default)]
    pub velocity: Vec2,
    /// Last movement direction the server accepted (for sprite facing).
    #[serde(default = "facing_default")]
    pub facing: Vec2,
    pub hp: u32,
    pub max_hp: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    #[serde(default)]
    pub score: i32,
    #[serde(default)]
    pub is_alive: bool,
}

impl PlayerState {
    pub fn hp_fraction(&self) -> f32 {
        if self.max_hp == 0 {
            0.0
        } else {
            (self.hp as f32 / self.max_hp as f32).clamp(0.0, 1.0)
        }
    }

    pub fn is_dead(&self) -> bool {
        !self.is_alive || self.hp == 0
    }
}

fn facing_default() -> Vec2 {
    Vec2::RIGHT
}

impl Default for PlayerState {
    fn default() -> Self {
        Self {
            id: PlayerId(String::new()),
            name: String::new(),
            position: Vec2::ZERO,
            velocity: Vec2::ZERO,
            // Default to facing right so sprites never render mirrored by accident.
            facing: facing_default(),
            hp: 100,
            max_hp: 100,
            team: None,
            score: 0,
            is_alive: true,
        }
    }
}

/// Round/match information mirrored from `RoundState` snapshots.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RoundInfo {
    pub id: Option<RoundId>,
    pub number: Option<u32>,
    pub time_left_ms: u32,
    pub winner: Option<String>,
}
