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
use crate::net::mock::MockTransport;
use crate::net::session;
use crate::net::transport::{MessageTransport, WsTransport};
use crate::protocol::message::{GameEvent, Hello, Phase, RoundState, Snapshot, Welcome};
use crate::protocol::validation::Validate;

/// Builds the connection future for the session driver (WebSocket transport).
fn websocket_connect(url: String) -> session::ConnectFn {
    Box::new(move || {
        let url = url.clone();
        Box::pin(async move {
            let transport: Box<dyn MessageTransport> =
                Box::new(WsTransport::connect_url(url).await?);
            Ok(transport)
        }) as session::BoxFuture
    })
}

/// Starts the session driver with a connection factory.
///
/// The factory has to be *polled* inside a runtime (its I/O driver lives
/// there). `GameClient` therefore either borrows the caller's runtime or passes
/// one it owns – that private runtime is shut down once the driver stops.
fn spawn_driver(
    connection: crate::config::ConnectionConfig,
    hello: Hello,
    connect: session::ConnectFn,
) -> anyhow::Result<NetClientHandle> {
    session::spawn_with_transports(connection, hello, connect)
}

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
    /// Hello currently in flight, so a manual reconnect can replay it.
    hello: Option<Hello>,
    /// Set once a `welcome` arrived; gates input during a half-open socket.
    handshake_complete: bool,
    /// Newest snapshot applied since the last [`GameClient::take_applied_snapshot`].
    ///
    /// The networking layer cannot touch the replay buffer or the predictor
    /// (they live on the game thread), so it hands the applied snapshot over
    /// instead. Only the newest matters for reconciliation: authority is always
    /// the latest server state.
    latest_snapshot: Option<Snapshot>,
    /// Mode packages received this session and not yet persisted by the app.
    received_modes: Vec<crate::protocol::message::ModePackage>,
    /// Runtime under which connection futures are *polled*.
    ///
    /// `None` uses the caller's ambient runtime (tests, embedding). Without one
    /// the driver refuses to connect with a clear error instead of hanging, so
    /// the game loop passes a runtime it owns.
    runtime: Option<tokio::runtime::Runtime>,
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
            hello: None,
            handshake_complete: false,
            latest_snapshot: None,
            received_modes: Vec::new(),
            runtime: None,
        }
    }

    /// Drives connections with `runtime` instead of the caller's runtime.
    pub fn with_runtime(mut self, runtime: tokio::runtime::Runtime) -> Self {
        self.runtime = Some(runtime);
        self
    }

    /// Keeps the driver's runtime alive (see [`GameClient::with_runtime`]).
    pub fn releases_runtime(&mut self) -> Option<tokio::runtime::Runtime> {
        self.runtime.take()
    }

    /// Starts the network driver in the background.
    pub fn connect(&mut self, hello: Hello) -> anyhow::Result<()> {
        let connection = self.config.connection.clone();
        self.start(hello, move |hello| {
            let connect = websocket_connect(connection.url.clone());
            spawn_driver(connection, hello, connect)
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
            let connect: session::ConnectFn = Box::new(move || {
                let server = server.clone();
                Box::pin(async move {
                    let transport: Box<dyn MessageTransport> =
                        Box::new(MockTransport::connect(server).await);
                    Ok(transport)
                }) as session::BoxFuture
            });
            spawn_driver(connection.clone(), hello, connect)
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
        self.hello = Some(hello.clone());
        let handle = spawn(hello)?;
        self.handle = Some(handle);
        self.interpolator.clear();
        self.handshake_complete = false;
        self.status = ConnectionStatus::Connecting;
        Ok(())
    }

    pub fn disconnect(&mut self) {
        if let Some(handle) = self.handle.take() {
            let _ = handle.shutdown();
        }
        self.status = ConnectionStatus::Disconnected;
        self.handshake_complete = false;
        self.interpolator.clear();
    }

    /// Stops the current driver and starts a fresh one with the previous hello.
    ///
    /// Needed when automatic retries are exhausted (`Retrying`) or the user
    /// wants to retry after a fatal server error.
    pub fn reconnect(&mut self) -> anyhow::Result<()> {
        let hello = self
            .hello
            .clone()
            .ok_or_else(|| anyhow::anyhow!("client was never connected"))?;
        let connection = self.config.connection.clone();
        self.start(hello, move |hello| {
            let connect = websocket_connect(connection.url.clone());
            spawn_driver(connection, hello, connect)
        })
    }

    pub fn status(&self) -> ConnectionStatus {
        self.status
    }

    /// The status as of the last [`GameClient::update`]. The session driver and
    /// the game thread race by one frame, so the HUD should use this value
    /// instead of [`GameClient::status`].
    pub fn is_connected(&self) -> bool {
        self.status.can_send_gameplay() && self.handshake_complete
    }

    /// Mode packages received since the last call (the app persists them).
    pub fn take_mode_packages(&mut self) -> Vec<crate::protocol::message::ModePackage> {
        std::mem::take(&mut self.received_modes)
    }

    /// Returns the newest snapshot applied since the last call.
    ///
    /// The game loop feeds it to `Predictor::on_snapshot`, which is what prunes
    /// acknowledged inputs from the replay buffer and updates the
    /// authoritative base position. Without this hand-over the buffer only
    /// ever grows and prediction has no authority.
    pub fn take_applied_snapshot(&mut self) -> Option<Snapshot> {
        self.latest_snapshot.take()
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

    /// Sends one non-gameplay protocol message (currently `pull`).
    pub fn send_message(&self, message: crate::protocol::message::ProtocolMessage) -> bool {
        if !self.is_connected() {
            return false;
        }
        match &self.handle {
            Some(handle) => handle.send_raw(message).is_ok(),
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
        let events = match &self.handle {
            Some(handle) => handle.drain_events(),
            None => Vec::new(),
        };
        self.apply_events(state, events)
    }

    /// Applies already-drained events – the seam tests use to drive
    /// `GameClient` without a socket.
    fn apply_events(
        &mut self,
        state: &mut crate::models::GameState,
        events: Vec<NetEvent>,
    ) -> NetworkTick {
        let mut tick = NetworkTick::default();

        for event in events {
            match event {
                NetEvent::Status(status) => {
                    if status != self.status {
                        tracing::info!(from = ?self.status, to = ?status, "connection status");
                    }
                    let was_connected = self.handshake_complete;
                    self.status = status;
                    state.connection = status;
                    self.handshake_complete = status.can_send_gameplay();

                    if !status.can_send_gameplay() {
                        // Authoritative state from an old session must not leak
                        // into the next one – neither into the interpolator nor
                        // into `GameState`. Predictions and local effects are
                        // render-only and go with it.
                        self.interpolator.clear();
                        self.latest_snapshot = None;
                        state.reset_session();
                        state.connection = status;
                        if was_connected {
                            tick.notifications.push("Connection lost".to_owned());
                            self.effects.enqueue(crate::effects::FxEvent::Toast(
                                "Connection lost".to_owned(),
                            ));
                        }
                    }
                }
                NetEvent::Welcome(welcome) => self.apply_welcome(state, welcome),
                NetEvent::Mode(payload) => match payload.mode {
                    Some(package) => {
                        tracing::info!(mode = %package.id, bytes = package.script.len(), "mode payload received");
                        self.received_modes.push(package);
                    }
                    None => tracing::info!("match runs the built-in game (no mode package)"),
                },
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
                    // Transport errors are frequent while the server is down
                    // (once per reconnect attempt); the session driver already
                    // logs them, here they only end up in the HUD.
                    tracing::debug!(%error, "transport error");
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
        state.mode = welcome.mode.clone();
        state.tick_rate = welcome.tick_rate;
        state.server_tick = Tick(welcome.server_tick);
        state.last_error = None;
        state.world = crate::models::WorldBounds::from_welcome(&welcome);
        state.move_speed = welcome.move_speed;
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
            // Still take the ack: it is per-player and monotonic, so it is
            // never stale just because the world state is. (The predictor
            // prunes the replay buffer with it in `on_snapshot`.)
            if let Some(acked) = snapshot.acked_input_sequence {
                self.effects.record_ack(acked);
                state.acked_input_sequence = Some(acked);
            }
            return;
        }

        // ---- authoritative data: server wins unconditionally ----
        state.server_tick = Tick(snapshot.tick);
        state.players.clear();
        for player in &snapshot.players {
            state.players.insert(player.id.clone(), player.clone());
        }
        // Markers belong to the mode script; the newest snapshot is the whole
        // truth, exactly like the player list.
        state.markers.clone_from(&snapshot.markers);
        if let Some(world) = snapshot.world {
            state.world = crate::models::WorldBounds::from_dimensions(world.width, world.height);
        }
        if let Some(round) = &snapshot.round {
            state.round.id = round.round_id.clone();
            state.round.time_left_ms = round.time_left_ms;
        }

        // ---- interpolation buffer (render-only) ----
        if let Some(acked) = snapshot.acked_input_sequence {
            self.effects.record_ack(acked);
            state.acked_input_sequence = Some(acked);
        }
        self.latest_snapshot = Some(snapshot.clone());
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
            GameEvent::ModeLog { level, message, .. } => {
                state.push_mode_log(level.clone(), message.clone());
                tracing::debug!(level = %level, %message, "mode script log");
            }
            GameEvent::ModeError { mode_id, message } => {
                tracing::error!(%mode_id, %message, "mode script failed");
                state.mode_error = Some(message.clone());
                state.push_mode_log("error", message.clone());
                tick.notifications
                    .push(format!("Mode script error: {message}"));
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
        let previous = state.phase;
        if round.phase != previous {
            tracing::info!(from = ?previous, to = ?round.phase, "phase change");
            self.effects.on_phase_changed(previous, round.phase);
        }
        state.phase = round.phase;
        state.round.id = round.round.clone().or(state.round.id.take());
        state.round.number = round.round_number.or(state.round.number);
        state.round.winner = round.winner.clone();

        // `countdown_ms` is only authoritative during the countdown. Outside of
        // it the server sends 0, which must not wipe the round timer that the
        // snapshots keep up to date.
        if round.phase == Phase::Countdown {
            if let Some(countdown) = round.countdown_ms {
                state.round.time_left_ms = countdown;
            }
        } else if matches!(round.phase, Phase::Lobby | Phase::Connecting) {
            state.round.time_left_ms = 0;
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
        ConnectionStatus::Reconnecting { .. } => "Reconnecting…".to_owned(),
        ConnectionStatus::Retrying => "Server unreachable – retrying".to_owned(),
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
            world: None,
            move_speed: None,
            mode: None,
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
    fn snapshots_carry_markers_of_the_running_mode() {
        let mut client = connected_client();
        let mut state = crate::models::GameState::new();
        let snapshot = Snapshot {
            tick: 5,
            players: vec![default_player("a")],
            markers: vec![crate::models::world::MarkerState {
                id: 3,
                x: 10.0,
                y: 20.0,
                label: "+1".to_owned(),
                kind: "coin".to_owned(),
            }],
            ..Default::default()
        };
        client.apply_snapshot(&mut state, snapshot);
        assert_eq!(state.markers.len(), 1);
        assert_eq!(state.markers[0].kind, "coin");

        // A newer snapshot without markers clears them (newest state wins).
        client.apply_snapshot(
            &mut state,
            Snapshot {
                tick: 6,
                players: vec![default_player("a")],
                ..Default::default()
            },
        );
        assert!(state.markers.is_empty());
    }

    #[test]
    fn invalid_markers_drop_the_whole_snapshot() {
        let mut client = connected_client();
        let mut state = crate::models::GameState::new();
        let snapshot = Snapshot {
            tick: 5,
            players: vec![default_player("a")],
            markers: vec![crate::models::world::MarkerState {
                id: 1,
                x: f32::NAN,
                y: 0.0,
                label: String::new(),
                kind: "coin".to_owned(),
            }],
            ..Default::default()
        };
        client.apply_snapshot(&mut state, snapshot);
        assert!(state.markers.is_empty());
        assert_eq!(state.server_tick, Tick::ZERO);
    }

    #[test]
    fn mode_events_fill_the_console_and_the_error() {
        let mut client = connected_client();
        let mut state = crate::models::GameState::new();
        let mut tick = NetworkTick::default();

        client.apply_game_event(
            &mut state,
            GameEvent::ModeLog {
                mode_id: "coin_rush".to_owned(),
                level: "log".to_owned(),
                message: "Coin collected (1/10)".to_owned(),
            },
            &mut tick,
        );
        assert_eq!(state.mode_log.len(), 1);
        assert!(state.mode_error.is_none());

        client.apply_game_event(
            &mut state,
            GameEvent::ModeError {
                mode_id: "coin_rush".to_owned(),
                message: "boom".to_owned(),
            },
            &mut tick,
        );
        assert_eq!(state.mode_error.as_deref(), Some("boom"));
        assert!(!tick.notifications.is_empty());
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

    #[test]
    fn running_round_state_keeps_the_snapshot_timer() {
        // Laravel sends `countdown_ms: 0` for every non-countdown phase; that
        // used to reset the round clock to 0:00 right after the start.
        let mut client = connected_client();
        let mut state = crate::models::GameState::new();
        state.round.time_left_ms = 179_000;
        client.apply_round_state(
            &mut state,
            RoundState {
                phase: Phase::Running,
                round: Some("lan-1".into()),
                round_number: Some(1),
                countdown_ms: None,
                winner: None,
            },
        );
        assert_eq!(state.round.time_left_ms, 179_000);
        assert_eq!(state.round.id, Some("lan-1".into()));

        client.apply_round_state(
            &mut state,
            RoundState {
                phase: Phase::Countdown,
                round: Some("lan-1".into()),
                round_number: Some(1),
                countdown_ms: Some(3000),
                winner: None,
            },
        );
        assert_eq!(state.round.time_left_ms, 3000);
    }

    #[test]
    fn losing_the_connection_resets_authoritative_state() {
        let mut client = connected_client();
        client.handshake_complete = true;
        let mut state = crate::models::GameState::new();
        state.local_player = Some("me".into());
        state.players.insert("a".into(), default_player("a"));
        self::state_connected(&mut client, &mut state);
        assert_eq!(state.connection, ConnectionStatus::Connected);

        let _ = client.apply_events(
            &mut state,
            vec![NetEvent::Status(ConnectionStatus::Reconnecting {
                attempt: 1,
            })],
        );

        assert!(state.players.is_empty(), "stale players must not survive");
        assert!(state.local_player.is_none());
        assert!(!client.is_connected());
    }

    #[test]
    fn connection_lost_is_announced_once() {
        let mut client = connected_client();
        client.status = ConnectionStatus::Connected;
        client.handshake_complete = true;
        let mut state = crate::models::GameState::new();
        state.connection = ConnectionStatus::Connected;
        let tick = client.apply_events(
            &mut state,
            vec![NetEvent::Status(ConnectionStatus::Reconnecting {
                attempt: 1,
            })],
        );
        assert!(
            tick.notifications
                .iter()
                .any(|n| n.contains("Connection lost")),
            "player should be told the connection dropped: {:?}",
            tick.notifications
        );
        // Toasts are drained separately from the HUD notification list.
        client.effects.drain();
        let tick = client.apply_events(
            &mut state,
            vec![NetEvent::Status(ConnectionStatus::Reconnecting {
                attempt: 2,
            })],
        );
        assert!(tick.notifications.is_empty(), "only announced once");
    }

    fn state_connected(client: &mut GameClient, state: &mut crate::models::GameState) {
        client.status = ConnectionStatus::Connected;
        client.handshake_complete = true;
        state.connection = ConnectionStatus::Connected;
    }
}
