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
use crate::protocol::message::{Phase, Welcome};

/// Playfield extents in world units.
///
/// The server owns the map; the client only needs the size for the camera and
/// for drawing the border. Until the welcome carries real dimensions the
/// Laravel defaults (`config/lanbash.php` → `gameplay.world`) are used.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WorldBounds {
    pub width: f32,
    pub height: f32,
}

impl Default for WorldBounds {
    fn default() -> Self {
        Self {
            width: 3200.0,
            height: 1800.0,
        }
    }
}

impl WorldBounds {
    pub fn from_dimensions(width: f32, height: f32) -> Self {
        if width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0 {
            Self { width, height }
        } else {
            Self::default()
        }
    }

    /// Uses `welcome.world` when the server announced one.
    pub fn from_welcome(welcome: &Welcome) -> Self {
        welcome
            .world
            .map(|world| Self::from_dimensions(world.width, world.height))
            .unwrap_or_default()
    }

    pub fn center(self) -> Vec2 {
        Vec2::new(self.width * 0.5, self.height * 0.5)
    }

    pub fn contains(self, point: Vec2) -> bool {
        (0.0..=self.width).contains(&point.x) && (0.0..=self.height).contains(&point.y)
    }
}

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
    /// Playfield extents (server-provided when available).
    pub world: WorldBounds,
    /// Server-announced movement speed (world units/s), used *only* for local
    /// visual prediction. `None` until the server sends it.
    pub move_speed: Option<f32>,
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
    /// Highest input sequence the server acknowledged, for diagnostics.
    pub acked_input_sequence: Option<u64>,
    /// How often the replay buffer overflowed (diagnostics; 0 is expected).
    pub buffer_overflows: u64,
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
            world: WorldBounds::default(),
            move_speed: None,
            latency_ms: 0.0,
            local_override: None,
            visuals: VisualState::default(),
            acked_input_sequence: None,
            buffer_overflows: 0,
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
        self.acked_input_sequence = None;
        self.buffer_overflows = 0;
        // `world` and `move_speed` are configuration, not session state: the
        // welcome re-sends them anyway, and keeping them avoids a frame with a
        // 0×0 map.
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
