//! Aggregate, read-mostly view of the match.
//!
//! [`GameState`] is the single object render/UI read from. It is updated in
//! exactly three places:
//! * `net::client::GameClient::apply_*` (authoritative data from the server)
//! * `app` (local session fields: connection status, our player id)
//! * `prediction` (the *visual* position of our own player, never the truth)
//!
//! Storing authoritative and predicted positions separately is intentional:
//! `GameState::player(position)` returns server truth, `visual` carries the
//! smoothed/predicted value used for drawing.

use std::collections::HashMap;

use crate::core::{MatchId, PlayerId, Tick, Vec2};
use crate::models::visual::VisualState;
use crate::models::world::{PlayerState, RoundInfo};
use crate::net::connection::ConnectionStatus;
use crate::protocol::message::Phase;

#[derive(Debug, Clone)]
pub struct GameState {
    pub connection: ConnectionStatus,
    /// Our own session, as assigned by the server.
    pub local_player: Option<PlayerId>,
    pub match_id: Option<MatchId>,
    pub phase: Phase,
    /// Authoritative players, keyed by id.
    pub players: HashMap<PlayerId, PlayerState>,
    pub round: RoundInfo,
    /// Last applied authoritative tick.
    pub server_tick: Tick,
    /// Tick rate announced by the server.
    pub tick_rate: u32,
    /// Measured round-trip latency in ms (smoothed).
    pub latency_ms: f32,
    /// **Render-only** predicted position of our own player.
    ///
    /// Written by `simulation::prediction` every tick and consumed by the
    /// renderer. Never read by networking, never sent to the server, and
    /// cleared whenever authority is lost. `players` always holds the truth.
    pub local_override: Option<Vec2>,
    /// Purely local animation state (never sent, never validated).
    pub visuals: VisualState,
    /// Last error from the server or transport, for the HUD.
    pub last_error: Option<String>,
}

impl Default for GameState {
    fn default() -> Self {
        Self {
            connection: ConnectionStatus::Disconnected,
            local_player: None,
            match_id: None,
            phase: Phase::Connecting,
            players: HashMap::new(),
            round: RoundInfo::default(),
            server_tick: Tick::ZERO,
            tick_rate: 60,
            latency_ms: 0.0,
            local_override: None,
            visuals: VisualState::default(),
            last_error: None,
        }
    }
}

impl GameState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn player(&self, id: &PlayerId) -> Option<&PlayerState> {
        self.players.get(id)
    }

    pub fn local_player(&self) -> Option<&PlayerState> {
        self.local_player
            .as_ref()
            .and_then(|id| self.players.get(id))
    }

    pub fn is_local(&self, id: &PlayerId) -> bool {
        self.local_player.as_ref() == Some(id)
    }

    /// Whether gameplay input should currently produce movement.
    pub fn accepts_gameplay_input(&self) -> bool {
        matches!(self.phase, Phase::Running)
    }

    /// Players sorted by id for stable rendering order (HashMap iteration is random).
    pub fn players_sorted(&self) -> Vec<&PlayerState> {
        let mut players: Vec<_> = self.players.values().collect();
        players.sort_by(|a, b| a.id.cmp(&b.id));
        players
    }

    pub fn is_connected(&self) -> bool {
        matches!(self.connection, ConnectionStatus::Connected)
    }

    /// Drops everything that is session-scoped when a connection is lost.
    /// Authoritative data must never survive into a new session.
    pub fn reset_session(&mut self) {
        self.local_player = None;
        self.match_id = None;
        self.phase = Phase::Connecting;
        self.players.clear();
        self.round = RoundInfo::default();
        self.visuals.clear();
        self.local_override = None;
        self.server_tick = Tick::ZERO;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn players_sorted_is_stable() {
        let mut state = GameState::new();
        for id in ["c", "a", "b"] {
            state.players.insert(
                PlayerId::from(id),
                PlayerState {
                    id: PlayerId::from(id),
                    ..Default::default()
                },
            );
        }
        let order: Vec<_> = state
            .players_sorted()
            .iter()
            .map(|p| p.id.0.as_str())
            .collect();
        assert_eq!(order, ["a", "b", "c"]);
    }

    #[test]
    fn reset_session_clears_authoritative_data() {
        let mut state = GameState::new();
        state.local_player = Some("me".into());
        state.server_tick = Tick(42);
        state.reset_session();
        assert!(state.local_player.is_none());
        assert_eq!(state.server_tick, Tick::ZERO);
    }
}
