//! In-memory transport that behaves like a miniature authoritative server.
//!
//! It exists so the *entire* client stack (session driver → handshake →
//! heartbeat → snapshot ingestion → prediction) can run without a server:
//!
//! * unit/integration tests get a deterministic server,
//! * `--headless` can run a smoke test in CI,
//! * `--mock-server` starts the GUI against a fake match, which is the fastest
//!   way to see the renderer and interpolator working.
//!
//! The mock server is deliberately dumb: constant velocity circles for a few
//! bots, no collisions, no authority subtleties. It is **not** a reference for
//! server behaviour – the game server remains the source of truth.

use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::core::{PlayerId, Vec2};
use crate::models::world::PlayerState;
use crate::net::transport::{MessageTransport, TransportError};
use crate::protocol::PROTOCOL_VERSION;
use crate::protocol::message::{GameEvent, Phase, ProtocolMessage, RoundState, Snapshot, Welcome};

/// Steps the fake simulation every `tick_ms` milliseconds.
const DEFAULT_TICK_MS: u64 = 50; // 20 Hz snapshots, like the real server.

#[derive(Debug, Clone)]
pub struct MockServerConfig {
    pub player_id: PlayerId,
    pub player_name: String,
    pub tick_rate: u32,
    /// Snapshot interval in milliseconds.
    pub snapshot_interval_ms: u64,
    pub bot_count: usize,
}

impl Default for MockServerConfig {
    fn default() -> Self {
        Self {
            player_id: PlayerId::from("local"),
            player_name: "Player".to_owned(),
            tick_rate: 60,
            snapshot_interval_ms: DEFAULT_TICK_MS,
            bot_count: 3,
        }
    }
}

/// The transport half handed to the session driver.
pub struct MockTransport {
    outbound: mpsc::UnboundedSender<ProtocolMessage>,
    inbound: mpsc::UnboundedReceiver<String>,
    closed: bool,
}

impl MockTransport {
    /// Connects to a fresh in-memory server.
    ///
    /// The fake server needs a runtime while the transport is created; it is
    /// spawned there and then keeps running on its **own thread**. It is never
    /// tied to a test or to a game thread, so tests that concurrently create
    /// (and drop) other runtimes cannot shut it down underneath the session.
    /// This used to be the cause of the "connected but zero players" flake.
    pub async fn connect(config: MockServerConfig) -> Self {
        let (outbound, outbound_rx) = mpsc::unbounded_channel();
        let (inbound_tx, inbound) = mpsc::unbounded_channel();

        let server_thread = std::thread::Builder::new()
            .name("lan-bash-mock-server".to_owned())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_time()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(err) => {
                        tracing::error!(%err, "failed to build mock server runtime");
                        return;
                    }
                };
                runtime.block_on(mock_server(config, outbound_rx, inbound_tx));
            });
        if let Err(err) = server_thread {
            tracing::error!(%err, "failed to start mock server thread");
        }

        Self {
            outbound,
            inbound,
            closed: false,
        }
    }
}

#[async_trait]
impl MessageTransport for MockTransport {
    async fn next_message(&mut self) -> Result<Option<String>, TransportError> {
        match self.inbound.recv().await {
            Some(payload) => Ok(Some(payload)),
            None => Ok(None),
        }
    }

    async fn send(&mut self, payload: String) -> Result<(), TransportError> {
        if self.closed {
            return Err(TransportError::Stopped);
        }
        let message = ProtocolMessage::from_json_str(&payload).map_err(|err| {
            TransportError::Io(format!("mock server received malformed message: {err}"))
        })?;
        self.outbound
            .send(message)
            .map_err(|_| TransportError::Stopped)
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        self.closed = true;
        Ok(())
    }
}

impl MockTransport {
    /// Pops the next server message – used by tests that drive the mock server
    /// directly, without the session driver in between.
    #[cfg(test)]
    pub(crate) async fn next_inbound(&mut self) -> Option<String> {
        self.next_message().await.ok().flatten()
    }
}

// ---------------------------------------------------------------------------
// Fake server
// ---------------------------------------------------------------------------

struct Bot {
    id: PlayerId,
    position: Vec2,
    angle: f32,
    radius: f32,
    speed: f32,
}

async fn mock_server(
    config: MockServerConfig,
    mut outbound: mpsc::UnboundedReceiver<ProtocolMessage>,
    inbound: mpsc::UnboundedSender<String>,
) {
    let mut tick: u64 = 0;
    let mut acknowledged_sequence: u64 = 0;
    let mut local_position = Vec2::new(120.0, 90.0);
    let mut bots: Vec<Bot> = (0..config.bot_count)
        .map(|index| Bot {
            id: PlayerId::from(format!("bot-{index}")),
            position: Vec2::new(60.0 + index as f32 * 40.0, 90.0),
            angle: index as f32 * 1.7,
            radius: 20.0 + index as f32 * 8.0,
            speed: 0.02 + index as f32 * 0.01,
        })
        .collect();

    let snapshot_period = Duration::from_millis(config.snapshot_interval_ms.max(10));
    let ticks_per_snapshot =
        ((config.tick_rate as u64 * config.snapshot_interval_ms.max(10)) / 1000).max(1);
    let mut welcome_sent = false;
    // `interval` (unlike a fresh `sleep` per iteration) keeps its deadline
    // across loop iterations. A client that sends an input every tick used to
    // win the `select!` every time and reset the sleep, so the fake server
    // never produced a snapshot ("connected but zero players").
    let mut snapshot_timer = tokio::time::interval(snapshot_period);
    snapshot_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            message = outbound.recv() => {
                let Some(message) = message else { break };
                match message {
                    ProtocolMessage::Hello(_) if !welcome_sent => {
                        welcome_sent = true;
                        let welcome = ProtocolMessage::Welcome(Welcome {
                            protocol_version: PROTOCOL_VERSION,
                            player_id: config.player_id.clone(),
                            session_id: Some("mock-session".into()),
                            match_id: None,
                            tick_rate: config.tick_rate,
                            server_tick: tick,
                            message: Some(format!("Welcome, {}", config.player_name)),
                            mode: None,
                            // Same world and speed as the default server.
                            world: Some(crate::protocol::message::WorldState {
                                width: 3200.0,
                                height: 1800.0,
                            }),
                            move_speed: Some(220.0),
                        });
                        let _ = send(&inbound, &welcome);
                        let _ = send(&inbound, &ProtocolMessage::RoundState(RoundState {
                            phase: Phase::Running,
                            round: None,
                            round_number: Some(1),
                            countdown_ms: None,
                            winner: None,
                        }));
                    }
                    ProtocolMessage::Hello(_) => {
                        // Duplicate hello – ignore.
                    }
                    ProtocolMessage::Input(input) => {
                        acknowledged_sequence = acknowledged_sequence.max(input.sequence);
                        // Deterministic, collision-free movement: just drift a bit.
                        local_position += input.movement * 0.5;
                        local_position.x = local_position.x.clamp(16.0, 464.0);
                        local_position.y = local_position.y.clamp(16.0, 254.0);
                    }
                    ProtocolMessage::Ping(ping) => {
                        let _ = send(&inbound, &ProtocolMessage::Pong(crate::protocol::message::Pong {
                            client_time_ms: ping.client_time_ms,
                            server_time_ms: None,
                        }));
                    }
                    ProtocolMessage::Unknown(_) => {}
                    other => {
                        tracing::debug!(kind = other.type_name(), "mock server ignoring message");
                    }
                }
            }
            _ = snapshot_timer.tick() => {
                if !welcome_sent {
                    continue;
                }

                tick += ticks_per_snapshot;
                for bot in &mut bots {
                    bot.angle += bot.speed;
                    // Circle around a center point using an ellipse.
                    bot.position = Vec2::new(240.0, 135.0)
                        + Vec2::new(bot.angle.cos() * (bot.radius * 3.0), bot.angle.sin() * bot.radius);
                }

                if tick.is_multiple_of(200)
                    && let Some(bot) = bots.first()
                {
                    let _ = send(&inbound, &ProtocolMessage::Event(GameEvent::PlayerHit {
                        attacker: config.player_id.clone(),
                        victim: bot.id.clone(),
                        damage: 7,
                        remaining_hp: Some(93),
                        position: Some(bot.position),
                    }));
                }

                let mut players = vec![PlayerState {
                    id: config.player_id.clone(),
                    name: config.player_name.clone(),
                    position: local_position,
                    facing: Vec2::RIGHT,
                    hp: 100,
                    max_hp: 100,
                    is_alive: true,
                    ..Default::default()
                }];
                players.extend(bots.iter().map(|bot| PlayerState {
                    id: bot.id.clone(),
                    name: format!("Bot {}", bot.id.as_str()),
                    position: bot.position,
                    facing: Vec2::RIGHT,
                    hp: 100,
                    max_hp: 100,
                    is_alive: true,
                    ..Default::default()
                }));

                let snapshot = ProtocolMessage::Snapshot(Snapshot {
                    tick,
                    acked_input_sequence: Some(acknowledged_sequence),
                    players,
                    round: None,
                    server_time_ms: None,
                    world: None,
                    markers: Vec::new(),
                });
                if send(&inbound, &snapshot).is_err() {
                    break;
                }
            }
        }
    }
}

fn send(inbound: &mpsc::UnboundedSender<String>, message: &ProtocolMessage) -> Result<(), ()> {
    let payload = message.to_json_string().map_err(|_| ())?;
    inbound.send(payload).map_err(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mock_server_answers_hello_with_welcome() {
        let mut transport = MockTransport::connect(MockServerConfig {
            bot_count: 1,
            ..Default::default()
        })
        .await;

        let hello = ProtocolMessage::Hello(crate::protocol::message::Hello::new("tester", None));
        transport
            .send(hello.to_json_string().unwrap())
            .await
            .unwrap();

        let raw = transport.next_inbound().await.expect("welcome");
        let message = ProtocolMessage::from_json_str(&raw).unwrap();
        let ProtocolMessage::Welcome(welcome) = message else {
            panic!("expected welcome, got {message:?}");
        };
        assert_eq!(welcome.player_id, PlayerId::from("local"));
        assert_eq!(welcome.protocol_version, PROTOCOL_VERSION);
    }

    #[tokio::test]
    async fn mock_server_streams_snapshots_after_welcome() {
        let mut transport = MockTransport::connect(MockServerConfig {
            snapshot_interval_ms: 10,
            bot_count: 0,
            ..Default::default()
        })
        .await;

        transport
            .send(r#"{"type":"hello","protocol_version":1,"player_name":"x"}"#.into())
            .await
            .unwrap();
        let _welcome = transport.next_inbound().await.unwrap();

        // The round state may come first; look for a snapshot within a few frames.
        for _ in 0..5 {
            let raw = transport.next_inbound().await.unwrap();
            if let ProtocolMessage::Snapshot(snapshot) =
                ProtocolMessage::from_json_str(&raw).unwrap()
            {
                assert!(!snapshot.players.is_empty());
                return;
            }
        }
        panic!("mock server never sent a snapshot");
    }
}
