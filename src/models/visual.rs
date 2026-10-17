//! Purely local presentation state.
//!
//! Nothing here is sent to the server and nothing here may influence gameplay.
//! It exists so the renderer looks alive between network updates: hit flashes,
//! walk-cycle phases, damage numbers, etc.

use std::collections::HashMap;

use crate::core::{PlayerId, Vec2};

/// Time source for local animation, driven by the frame clock.
#[derive(Debug, Clone, Default)]
pub struct AnimationClock {
    /// Seconds since the client started.
    pub elapsed: f32,
}

impl AnimationClock {
    pub fn tick(&mut self, frame_time: f32) {
        self.elapsed += frame_time.max(0.0);
    }

    /// 0..1 oscillation used for idle bobbing; `seed` desynchronises players.
    pub fn bob(&self, seed: f32, speed: f32) -> f32 {
        ((self.elapsed * speed + seed).sin() * 0.5 + 0.5).clamp(0.0, 1.0)
    }

    /// 0..1 looping cycle for walk animations.
    pub fn cycle(&self, speed: f32) -> f32 {
        (self.elapsed * speed).fract().abs()
    }
}

/// Per-player animation state.
#[derive(Debug, Clone, Default)]
pub struct PlayerVisuals {
    /// Remaining hit-flash time in seconds.
    pub hit_flash: f32,
    /// Last known (interpolated) position, used for smooth transitions when a
    /// player appears in a snapshot at a slightly different spot.
    pub render_position: Option<Vec2>,
    /// Damage numbers floating up after a confirmed hit.
    pub damage_popups: Vec<DamagePopup>,
    /// Whether the player was moving in the last frame (drives walk cycle).
    pub moving: bool,
    /// Seed so idle animations are not synchronised across players.
    pub animation_seed: f32,
}

#[derive(Debug, Clone)]
pub struct DamagePopup {
    pub amount: u32,
    pub position: Vec2,
    pub age: f32,
    pub lifetime: f32,
}

impl DamagePopup {
    pub fn new(amount: u32, position: Vec2) -> Self {
        Self {
            amount,
            position,
            age: 0.0,
            lifetime: 1.1,
        }
    }

    pub fn progress(&self) -> f32 {
        (self.age / self.lifetime).clamp(0.0, 1.0)
    }

    pub fn is_finished(&self) -> bool {
        self.age >= self.lifetime
    }
}

/// Collection of all local animation state for the match.
#[derive(Debug, Clone, Default)]
pub struct VisualState {
    pub clock: AnimationClock,
    players: HashMap<PlayerId, PlayerVisuals>,
    /// Screen shake magnitude, decays over time. Used on hits near the camera.
    pub screen_shake: f32,
}

impl VisualState {
    pub fn player(&self, id: &PlayerId) -> Option<&PlayerVisuals> {
        self.players.get(id)
    }

    pub fn player_mut(&mut self, id: &PlayerId) -> &mut PlayerVisuals {
        self.players
            .entry(id.clone())
            .or_insert_with(|| PlayerVisuals {
                // Deterministic per-player seed keeps animations from syncing.
                animation_seed: (id.0.bytes().map(u32::from).sum::<u32>() % 1000) as f32 * 0.01,
                ..Default::default()
            })
    }

    /// Advances every animation by `frame_time` seconds and removes finished popups.
    pub fn update(&mut self, frame_time: f32) {
        self.clock.tick(frame_time);
        self.screen_shake = (self.screen_shake - frame_time * 12.0).max(0.0);

        for visuals in self.players.values_mut() {
            visuals.hit_flash = (visuals.hit_flash - frame_time).max(0.0);
            for popup in &mut visuals.damage_popups {
                popup.age += frame_time;
            }
            visuals.damage_popups.retain(|p| !p.is_finished());
        }
    }

    pub fn clear(&mut self) {
        self.players.clear();
        self.screen_shake = 0.0;
    }

    /// Drops animation state for players the server no longer reports.
    ///
    /// Keeps memory bounded during long matches with many joins/leaves.
    pub fn prune(&mut self, alive: &std::collections::HashSet<&PlayerId>) {
        self.players
            .retain(|id, _| alive.contains(id) || alive.is_empty());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn popups_expire() {
        let id = PlayerId::from("p");
        let mut visuals = VisualState::default();
        visuals
            .player_mut(&id)
            .damage_popups
            .push(DamagePopup::new(12, Vec2::ZERO));

        visuals.update(0.5);
        assert_eq!(visuals.player(&id).unwrap().damage_popups.len(), 1);
        visuals.update(1.0);
        assert!(visuals.player(&id).unwrap().damage_popups.is_empty());
    }

    #[test]
    fn seeds_differ_by_player() {
        let mut visuals = VisualState::default();
        let a = visuals.player_mut(&"a".into()).animation_seed;
        let b = visuals.player_mut(&"b".into()).animation_seed;
        // Seeds are derived from the id; "a" (97) vs "b" (98) differ by 0.01.
        assert_ne!(a, b);
    }
}
