//! Local feedback driven by *confirmed* server events.
//!
//! The client must never decide that a hit happened; it only *reacts* to a
//! `GameEvent::PlayerHit` from the server. [`Effects`] collects those reactions
//! as backend-agnostic [`FxEvent`]s so rendering and audio can consume them
//! independently, and it updates purely-local visual state (hit flash, damage
//! popups, screen shake).

use std::collections::VecDeque;

use crate::models::GameState;
use crate::models::visual::DamagePopup;
use crate::protocol::message::GameEvent;

/// Something the presentation layers should do right now.
#[derive(Debug, Clone, PartialEq)]
pub enum FxEvent {
    /// Play a one-shot sound.
    Sound(SoundCue),
    /// Spawn particles at a world position (renderer decides how many).
    Particles {
        position: crate::core::Vec2,
        color: ParticleColor,
        count: u32,
    },
    /// Small camera shake.
    Shake { magnitude: f32 },
    /// Show a HUD message.
    Toast(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SoundCue {
    Hit,
    HitLocal,
    PlayerJoined,
    PlayerLeft,
    RoundStart,
    RoundEnd,
    Countdown,
    ItemPickup,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParticleColor {
    Blood,
    Heal,
    Sparkle,
}

/// Maximum number of queued effects; a burst of hits must not grow unbounded.
const MAX_QUEUED: usize = 256;

#[derive(Debug, Default)]
pub struct Effects {
    queue: VecDeque<FxEvent>,
    /// Highest input sequence acknowledged by the server, for diagnostics.
    last_acked_sequence: Option<u64>,
}

impl Effects {
    pub fn record_ack(&mut self, sequence: u64) {
        self.last_acked_sequence = Some(sequence);
    }

    pub fn last_acked_sequence(&self) -> Option<u64> {
        self.last_acked_sequence
    }

    /// Reacts to an authoritative gameplay event.
    ///
    /// `state` is only used to update **visual** fields and to find the
    /// player's position for particle placement.
    pub fn on_game_event(&mut self, state: &mut GameState, event: &GameEvent) {
        match event {
            GameEvent::PlayerHit {
                victim,
                damage,
                position,
                remaining_hp,
                ..
            } => {
                self.enqueue(FxEvent::Sound(if state.is_local(victim) {
                    SoundCue::HitLocal
                } else {
                    SoundCue::Hit
                }));

                let position = (*position).or_else(|| state.player(victim).map(|p| p.position));
                if let Some(position) = position {
                    self.enqueue(FxEvent::Particles {
                        position,
                        color: ParticleColor::Blood,
                        count: 6,
                    });
                    let visuals = state.visuals.player_mut(victim);
                    visuals.hit_flash = 0.18;
                    visuals
                        .damage_popups
                        .push(DamagePopup::new(*damage, position));
                }

                let near_local =
                    state
                        .local_player()
                        .zip(position)
                        .is_some_and(|(local, hit_position)| {
                            local.position.distance(hit_position) < 8.0
                        });
                if near_local {
                    state.visuals.screen_shake = 3.0;
                    self.enqueue(FxEvent::Shake { magnitude: 3.0 });
                }

                if let Some(hp) = remaining_hp {
                    tracing::debug!(%victim, hp, "victim hp after hit");
                }
            }
            GameEvent::PlayerJoined { name, .. } => {
                self.enqueue(FxEvent::Sound(SoundCue::PlayerJoined));
                self.enqueue(FxEvent::Toast(format!("{name} joined")));
            }
            GameEvent::PlayerLeft { player_id } => {
                self.enqueue(FxEvent::Sound(SoundCue::PlayerLeft));
                self.enqueue(FxEvent::Toast(format!("{player_id} left")));
            }
            GameEvent::ItemPicked { player_id, item_id } => {
                self.enqueue(FxEvent::Sound(SoundCue::ItemPickup));
                if let (true, Some(player)) = (state.is_local(player_id), state.player(player_id)) {
                    self.enqueue(FxEvent::Particles {
                        position: player.position,
                        color: ParticleColor::Sparkle,
                        count: 8,
                    });
                }
                tracing::debug!(%player_id, %item_id, "item pickup");
            }
            GameEvent::ScoreChanged { .. }
            | GameEvent::ModeLog { .. }
            | GameEvent::ModeLogClear { .. }
            | GameEvent::ModeError { .. }
            | GameEvent::Unknown => {}
        }
    }

    /// Called on phase transitions so lobby/countdown sounds fire once.
    pub fn on_phase_changed(
        &mut self,
        from: crate::protocol::message::Phase,
        to: crate::protocol::message::Phase,
    ) {
        use crate::protocol::message::Phase;
        match (from, to) {
            (_, Phase::Countdown) => self.enqueue(FxEvent::Sound(SoundCue::Countdown)),
            (_, Phase::Running) => self.enqueue(FxEvent::Sound(SoundCue::RoundStart)),
            (Phase::Running, Phase::Finished) => self.enqueue(FxEvent::Sound(SoundCue::RoundEnd)),
            _ => {}
        }
    }

    pub fn enqueue(&mut self, event: FxEvent) {
        if self.queue.len() >= MAX_QUEUED {
            self.queue.pop_front();
        }
        self.queue.push_back(event);
    }

    /// Returns all queued effects, oldest first, and empties the queue.
    pub fn drain(&mut self) -> Vec<FxEvent> {
        self.queue.drain(..).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Vec2;
    use crate::models::world::PlayerState;

    fn state_with(player: &str) -> GameState {
        let mut state = GameState::new();
        state.local_player = Some("me".into());
        for id in ["me", player] {
            state.players.insert(
                id.into(),
                PlayerState {
                    id: id.into(),
                    name: id.to_owned(),
                    position: Vec2::new(100.0, 100.0),
                    ..Default::default()
                },
            );
        }
        state
    }

    #[test]
    fn local_hit_uses_local_sound_and_shakes() {
        let mut state = state_with("enemy");
        state.local_player = Some("enemy".into());
        let mut effects = Effects::default();
        effects.on_game_event(
            &mut state,
            &GameEvent::PlayerHit {
                attacker: "me".into(),
                victim: "enemy".into(),
                damage: 5,
                remaining_hp: Some(95),
                position: None,
            },
        );

        let drained = effects.drain();
        assert!(drained.contains(&FxEvent::Sound(SoundCue::HitLocal)));
        assert!(drained.iter().any(|e| matches!(e, FxEvent::Shake { .. })));
        assert!(state.visuals.screen_shake > 0.0);
    }

    #[test]
    fn queue_is_bounded() {
        let mut effects = Effects::default();
        for _ in 0..(MAX_QUEUED + 10) {
            effects.enqueue(FxEvent::Sound(SoundCue::Hit));
        }
        assert_eq!(effects.drain().len(), MAX_QUEUED);
    }
}
