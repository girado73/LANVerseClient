//! The game-facing networking facade.
//!
//! [`GameClient`] is what the rest of the game talks to. It wraps a
//! [`NetClientHandle`] and owns everything network-related that has to be
//! applied *in order* on the game thread:
//!
//! * it drains raw [`NetEvent`]s every frame,
//! * **validates** protocol data before it touches state,
//! * writes authoritative values into [`GameState`],
//! * pushes snapshots into the [`Interpolator`](crate::interpolation::Interpolator),
//! * hands gameplay events to [`Effects`](crate::effects::Effects) and UI,
//! * exposes typed `send_*` methods so gameplay code never builds JSON itself.
//!
//! Rusty / non-authoritative values (prediction, animation) live elsewhere and
//! are never written back into the server-mirrored fields.

use crate::config::NetConfig;
use crate::core::{PlayerId, Tick};
use crate::effects::Effects;
use crate::interpolation::Interpolator;
use crate::models::world::{PlayerState, RoundInfo};
use crate::net::connection::{ConnectionStatus, NetClientHandle};
use crate::net::events::NetEvent;
use crate::net::session;
use crate::protocol::message::{GameEvent, Hello, Phase, RoundState, Snapshot, Welcome};
use crate::protocol::validation::Validate;

/// Everything the game observes about networking in one frame.
#[derive(Debug, Default)]
pub struct NetworkTick {
    /// Events that the UI layer should react to (chat-like notifications, sounds).
    pub notifications: Vec<String>,
}

pub struct GameClient {
    handle: Option<NetClientHandle>,
    status: ConnectionStatus,
    config: NetConfig,
    interpolator: Interpolator,
    pub effects: Effects,
}

impl GameClient {
    /// Creates a client without a connection. Gameplay keeps running offline
    /// (rendering the last known state) until [`GameClient::connect`].
    pub fn new(config: NetConfig) -> Self {
        let interpolator = Interpolator::new(config.interpolation_buffer_size);
        Self {
            handle: None,
            status: ConnectionStatus::Disconnected,
            config,
            interpolator,
            effects: Effects::default(),
        }
    }

    /// Starts the network driver in the background.
    pub fn connect(&mut self, hello: Hello) -> anyhow::Result<()> {
        let connection = self.config.connection.clone();
        self.start(hello, move |hello| {
            session::spawn(connection.clone(), hello)
        })
    }

    /// Starts the driver against the in-memory mock server (tests, demos).
    pub fn connect_mock(
        &mut self,
        hello: Hello,
        server: crate::net::mock::MockServerConfig,
    ) -> anyhow::Result<()> {
        let connection = self.config.connection.clone();
        self.start(hello, move |hello| {
            session::spawn_with(connection.clone(), hello, move || {
                let server = server.clone();
                async move { Ok(crate::net::mock::MockTransport::connect(server).await) }
            })
        })
    }

    fn start(
        &mut self,
        hello: Hello,
        spawn: impl FnOnce(Hello) -> anyhow::Result<NetClientHandle>,
    ) -> anyhow::Result<()> {
        if let Some(handle) = &self.handle {
            let _ = handle.shutdown();
        }
        let handle = spawn(hello)?;
        self.handle = Some(handle);
        self.interpolator.clear();
        self.status = ConnectionStatus::Connecting;
        Ok(())
    }

    pub fn disconnect(&mut self) {
        if let Some(handle) = self.handle.take() {
            let _ = handle.shutdown();
        }
        self.status = ConnectionStatus::Disconnected;
        self.interpolator.clear();
    }

    pub fn status(&self) -> ConnectionStatus {
        self.status
    }

    pub fn is_connected(&self) -> bool {
        self.status.can_send_gameplay()
    }

    /// Sends an input message produced by [`InputPipeline::sample`].
    pub fn send_input_message(&self, message: crate::protocol::message::InputMessage) -> bool {
        if !self.is_connected() {
            return false;
        }
        match &self.handle {
            Some(handle) => handle.send_input(message).is_ok(),
            None => false,
        }
    }

    pub fn send_ping(&self) {
        if let Some(handle) = &self.handle {
            let _ = handle.send_ping();
        }
    }

    /// Drains all inbound network events and applies them to `state`.
    ///
    /// Called exactly once per frame by the app. Any event that would replace
    /// authoritative state is validated first; invalid frames are dropped.
    pub fn update(&mut self, state: &mut crate::models::GameState) -> NetworkTick {
        let mut tick = NetworkTick::default();

        let events = match &self.handle {
            Some(handle) => handle.drain_events(),
            None => Vec::new(),
        };

        for event in events {
            match event {
                NetEvent::Status(status) => {
                    if status != self.status {
                        tracing::info!(from = ?self.status, to = ?status, "connection status");
                    }
                    self.status = status;
                    state.connection = status;

                    if !status.can_send_gameplay() {
                        // Authoritative state from an old session must not leak
                        // into the next one.
                        self.interpolator.clear();
                    }
                }
                NetEvent::Welcome(welcome) => self.apply_welcome(state, welcome),
                NetEvent::Snapshot(snapshot) => self.apply_snapshot(state, snapshot),
                NetEvent::Game(event) => {
                    self.apply_game_event(state, event, &mut tick);
                }
                NetEvent::RoundState(round) => self.apply_round_state(state, round),
                NetEvent::Latency(rtt) => {
                    state.latency_ms = rtt;
                }
                NetEvent::ServerError(error) => {
                    tracing::warn!(code = %error.code, message = %error.message, fatal = error.fatal, "server error");
                    state.last_error = Some(error.message.clone());
                    tick.notifications
                        .push(format!("Server: {}", error.message));
                }
                NetEvent::TransportError(error) => {
                    tracing::warn!(%error, "transport error");
                    state.last_error = Some(error);
                }
            }
        }

        state.connection = self.status;
        tick
    }

    fn apply_welcome(&mut self, state: &mut crate::models::GameState, welcome: Welcome) {
        if let Err(err) = welcome.validate() {
            tracing::error!(%err, "rejecting welcome");
            state.last_error = Some(err.to_string());
            return;
        }

        state.local_player = Some(welcome.player_id.clone());
        state.match_id = welcome.match_id.clone();
        state.tick_rate = welcome.tick_rate;
        state.server_tick = Tick(welcome.server_tick);
        state.last_error = None;
        self.interpolator.clear();

        tracing::info!(
            player = %welcome.player_id,
            tick_rate = welcome.tick_rate,
            server_tick = welcome.server_tick,
            "welcome received"
        );
    }

    fn apply_snapshot(&mut self, state: &mut crate::models::GameState, snapshot: Snapshot) {
        if let Err(err) = snapshot.validate() {
            tracing::warn!(%err, tick = snapshot.tick, "dropping invalid snapshot");
            return;
        }
        if snapshot.tick < state.server_tick.0 {
            tracing::debug!(
                tick = snapshot.tick,
                current = state.server_tick.0,
                "dropping out-of-order snapshot"
            );
            return;
        }

        // ---- authoritative data: server wins unconditionally ----
        state.server_tick = Tick(snapshot.tick);
        state.players.clear();
        for player in &snapshot.players {
            state.players.insert(player.id.clone(), player.clone());
        }
        if let Some(round) = &snapshot.round {
            state.round.id = round.round_id.clone();
            state.round.time_left_ms = round.time_left_ms;
        }

        // ---- interpolation buffer (render-only) ----
        if let Some(acked) = snapshot.acked_input_sequence {
            self.effects.record_ack(acked);
        }
        self.interpolator.push(snapshot);

        // If a player vanished from the snapshot, their local visuals go too.
        let alive: std::collections::HashSet<&PlayerId> = state.players.keys().collect();
        state.visuals.prune(&alive);
    }

    fn apply_game_event(
        &mut self,
        state: &mut crate::models::GameState,
        event: GameEvent,
        tick: &mut NetworkTick,
    ) {
        match &event {
            GameEvent::PlayerJoined { player_id, name } => {
                tracing::info!(player = %player_id, %name, "player joined");
                tick.notifications.push(format!("{name} joined"));
            }
            GameEvent::PlayerLeft { player_id } => {
                tracing::info!(player = %player_id, "player left");
                let name = state
                    .player(player_id)
                    .map(|p| p.name.clone())
                    .unwrap_or_else(|| player_id.0.clone());
                tick.notifications.push(format!("{name} left"));
            }
            GameEvent::PlayerHit {
                attacker,
                victim,
                damage,
                ..
            } => {
                tracing::debug!(%attacker, %victim, damage, "hit confirmed");
            }
            GameEvent::ItemPicked { player_id, item_id } => {
                tracing::debug!(%player_id, %item_id, "item picked");
            }
            GameEvent::ScoreChanged {
                player_id,
                score,
                team,
            } => {
                tracing::debug!(%player_id, score, ?team, "score changed");
            }
            GameEvent::Unknown => {
                tracing::debug!("ignoring unknown game event");
                return;
            }
        }

        // Local presentation effects are derived from confirmed events only.
        self.effects.on_game_event(state, &event);
    }

    fn apply_round_state(&mut self, state: &mut crate::models::GameState, round: RoundState) {
        if round.phase != state.phase {
            tracing::info!(from = ?state.phase, to = ?round.phase, "phase change");
        }
        state.phase = round.phase;
        state.round.id = round.round.or(state.round.id.take());
        state.round.number = round.round_number.or(state.round.number);
        state.round.winner = round.winner.clone();
        if let Some(countdown) = round.countdown_ms {
            state.round.time_left_ms = countdown;
        }
    }

    /// The interpolator, for the renderer.
    pub fn interpolator(&mut self) -> &mut Interpolator {
        &mut self.interpolator
    }
}

impl Drop for GameClient {
    fn drop(&mut self) {
        self.disconnect();
    }
}

/// Human-readable connection status for the HUD.
pub fn describe_status(status: ConnectionStatus) -> String {
    match status {
        ConnectionStatus::Disconnected => "Not connected".to_owned(),
        ConnectionStatus::Connecting => "Connecting…".to_owned(),
        ConnectionStatus::Handshaking => "Handshaking…".to_owned(),
        ConnectionStatus::Connected => "Online".to_owned(),
        ConnectionStatus::Reconnecting { attempt } => format!("Reconnecting (attempt {attempt})…"),
        ConnectionStatus::Failed => "Connection failed".to_owned(),
    }
}

/// Convenience for the HUD: turns the phase into a display string.
pub fn phase_label(phase: Phase) -> &'static str {
    match phase {
        Phase::Connecting => "Connecting",
        Phase::Lobby => "Lobby – waiting for players",
        Phase::Countdown => "Get ready!",
        Phase::Running => "Round running",
        Phase::Finished => "Round finished",
    }
}

/// Applies a `RoundInfo` onto `GameState` – used by tests and future lobby UI.
pub fn merge_round_info(state: &mut crate::models::GameState, info: RoundInfo) {
    state.round = info;
}

/// Builds a `PlayerState` with defaults for tests.
pub fn default_player(id: &str) -> PlayerState {
    default_player_at(id, crate::core::Vec2::ZERO)
}

/// Builds a `PlayerState` at `position` with defaults, for tests.
pub fn default_player_at(id: &str, position: crate::core::Vec2) -> PlayerState {
    PlayerState {
        id: PlayerId::from(id),
        name: id.to_owned(),
        position,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Vec2;
    use crate::protocol::message::Welcome;
    fn connected_client() -> GameClient {
        let mut client = GameClient::new(NetConfig::default());
        client.status = ConnectionStatus::Connected;
        client
    }

    fn welcome(tick_rate: u32) -> Welcome {
        Welcome {
            protocol_version: crate::protocol::PROTOCOL_VERSION,
            player_id: "me".into(),
            session_id: None,
            match_id: None,
            tick_rate,
            server_tick: 10,
            message: None,
        }
    }

    #[test]
    fn welcome_sets_identity_and_tick_rate() {
        let mut client = connected_client();
        let mut state = crate::models::GameState::new();
        client.apply_welcome(&mut state, welcome(60));

        assert_eq!(state.local_player, Some("me".into()));
        assert_eq!(state.tick_rate, 60);
        assert_eq!(state.server_tick, Tick(10));
    }

    #[test]
    fn incompatible_welcome_is_rejected() {
        let mut client = connected_client();
        let mut state = crate::models::GameState::new();
        let mut bad = welcome(60);
        bad.protocol_version = 999;
        client.apply_welcome(&mut state, bad);

        assert!(state.local_player.is_none());
        assert!(state.last_error.is_some());
    }

    #[test]
    fn snapshot_replaces_players_and_ignores_older_ticks() {
        let mut client = connected_client();
        let mut state = crate::models::GameState::new();
        state.server_tick = Tick(20);

        let newer = Snapshot {
            tick: 30,
            players: vec![default_player("a")],
            ..Default::default()
        };
        client.apply_snapshot(&mut state, newer);
        assert_eq!(state.server_tick, Tick(30));
        assert_eq!(state.players.len(), 1);

        let older = Snapshot {
            tick: 25,
            players: vec![default_player("b")],
            ..Default::default()
        };
        client.apply_snapshot(&mut state, older);
        // Still the newer snapshot's player.
        assert!(state.players.contains_key(&PlayerId::from("a")));
        assert_eq!(state.server_tick, Tick(30));
    }

    #[test]
    fn hit_event_creates_local_effect() {
        let mut client = connected_client();
        let mut state = crate::models::GameState::new();
        state.players.insert("v".into(), default_player("v"));
        let mut tick = NetworkTick::default();

        client.apply_game_event(
            &mut state,
            GameEvent::PlayerHit {
                attacker: "a".into(),
                victim: "v".into(),
                damage: 12,
                remaining_hp: Some(88),
                position: Some(Vec2::new(1.0, 2.0)),
            },
            &mut tick,
        );

        let visuals = state.visuals.player(&PlayerId::from("v")).unwrap();
        assert!(visuals.hit_flash > 0.0);
        assert_eq!(visuals.damage_popups.len(), 1);
    }

    #[test]
    fn round_state_transitions_phase() {
        let mut client = connected_client();
        let mut state = crate::models::GameState::new();
        client.apply_round_state(
            &mut state,
            RoundState {
                phase: Phase::Running,
                round: None,
                round_number: Some(2),
                countdown_ms: None,
                winner: None,
            },
        );
        assert_eq!(state.phase, Phase::Running);
        assert_eq!(state.round.number, Some(2));
        assert!(state.accepts_gameplay_input());
    }
}
