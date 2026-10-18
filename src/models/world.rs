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
    /// The server does send velocity; it is display-only (interpolation and
    /// the run/idle animation), never used for movement decisions.
    #[serde(default)]
    pub velocity: Vec2,
    /// Last movement direction the server accepted (for sprite facing).
    #[serde(default = "facing_default")]
    pub facing: Vec2,
    /// Assume a sane 100 HP when the field is missing.
    #[serde(default = "default_max_hp")]
    pub max_hp: u32,
    #[serde(default = "default_max_hp")]
    pub hp: u32,
    /// `null` for free-for-all modes – the server always sends the key.
    #[serde(default, deserialize_with = "null_as_none")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    #[serde(default)]
    pub score: i32,
    #[serde(default = "default_true")]
    pub is_alive: bool,
}

fn default_max_hp() -> u32 {
    100
}

fn default_true() -> bool {
    true
}

/// Accepts both an absent and an explicit `null` value as `None`.
fn null_as_none<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer)
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

impl PlayerState {
    /// Whether the server reports this player as moving. Display-only.
    pub fn is_moving(&self) -> bool {
        self.is_alive && self.velocity.length_squared() > 1.0
    }
}

/// A marker placed by a Lua mode script (coin, zone, goal …).
///
/// Purely decorative on the client: the script decides what a marker means and
/// only the server may create, move or remove it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MarkerState {
    pub id: u64,
    pub x: f32,
    pub y: f32,
    #[serde(default)]
    pub label: String,
    /// Free-form kind ("coin", "goal", …) used to pick a colour.
    #[serde(default)]
    pub kind: String,
}

/// Round/match information mirrored from `RoundState` snapshots.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RoundInfo {
    pub id: Option<RoundId>,
    pub number: Option<u32>,
    pub time_left_ms: u32,
    pub winner: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nulls_and_missing_fields_deserialize() {
        let raw = r#"{"id":"p-1","name":"Alice","position":{"x":1,"y":2},
                      "velocity":{"x":0,"y":0},"facing":{"x":1,"y":0},
                      "hp":100,"max_hp":100,"is_alive":true,"score":0,"team":null}"#;
        let player: PlayerState = serde_json::from_str(raw).unwrap();
        assert_eq!(player.team, None);
        assert!(player.is_alive);

        // Minimal payload: hp/max_hp/is_alive fall back to healthy defaults.
        let raw = r#"{"id":"p-2","position":{"x":0,"y":0}}"#;
        let player: PlayerState = serde_json::from_str(raw).unwrap();
        assert_eq!(player.hp, 100);
        assert_eq!(player.max_hp, 100);
        assert!(player.is_alive);
    }

    #[test]
    fn movement_is_display_only() {
        let mut player = PlayerState::default();
        assert!(!player.is_moving());
        player.velocity = Vec2::new(120.0, 0.0);
        assert!(player.is_moving());
        player.is_alive = false;
        assert!(!player.is_moving());
    }
}
