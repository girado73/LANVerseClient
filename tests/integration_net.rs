//! End-to-end tests across module boundaries.
//!
//! These go one level above unit tests: they wire the real `GameClient`,
//! session driver and prediction together against the in-memory mock server,
//! so the handshake, snapshot ingestion and reconciliation paths are proven
//! to cooperate – not just their individual pieces.

use std::time::Duration;

use lan_bash_client::app::ClientApp;
use lan_bash_client::config::ClientConfig;
use lan_bash_client::config::NetConfig;
use lan_bash_client::core::Vec2;
use lan_bash_client::input::InputBuffer;
use lan_bash_client::models::GameState;
use lan_bash_client::net::client::GameClient;
use lan_bash_client::net::connection::ConnectionStatus;
use lan_bash_client::net::mock::MockServerConfig;
use lan_bash_client::protocol::message::{Hello, InputMessage, Phase};
use lan_bash_client::simulation::prediction::{MovementModel, Predictor};

/// Pumps `client.update` until `done` is true or the timeout elapses.
fn pump_until(
    client: &mut GameClient,
    state: &mut GameState,
    timeout: Duration,
    done: impl Fn(&GameState) -> bool,
) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        client.update(state);
        if done(state) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    false
}

#[test]
fn handshake_and_snapshots_reach_game_state() {
    let mut client = GameClient::new(NetConfig::default());
    let mut state = GameState::new();

    client
        .connect_mock(
            Hello::new("integration", None),
            MockServerConfig {
                player_name: "integration".into(),
                bot_count: 2,
                snapshot_interval_ms: 20,
                ..Default::default()
            },
        )
        .expect("mock driver starts");

    assert!(
        pump_until(&mut client, &mut state, Duration::from_secs(3), |s| {
            s.is_connected() && s.players.len() == 3
        }),
        "expected to connect and receive 3 players, got {:?}/{:?}",
        state.connection,
        state.players.len()
    );

    // Identity, tick rate and phase all come from the server.
    assert_eq!(
        state.local_player.as_ref().map(|p| p.as_str()),
        Some("local")
    );
    assert_eq!(state.tick_rate, 60);
    assert_eq!(state.phase, Phase::Running);
    assert!(state.server_tick.0 > 0);
}

#[test]
fn latency_measurement_round_trips_through_the_session() {
    let mut client = GameClient::new(NetConfig::default());
    let mut state = GameState::new();

    client
        .connect_mock(Hello::new("latency", None), MockServerConfig::default())
        .unwrap();

    assert!(pump_until(
        &mut client,
        &mut state,
        Duration::from_secs(3),
        |s| { s.connection == ConnectionStatus::Connected }
    ));
    assert!(
        pump_until(&mut client, &mut state, Duration::from_secs(3), |s| s
            .latency_ms
            >= 0.0),
        "no latency sample"
    );
}

#[test]
fn inputs_sent_through_game_client_are_acknowledged() {
    let mut client = GameClient::new(NetConfig::default());
    let mut state = GameState::new();

    client
        .connect_mock(Hello::new("sender", None), MockServerConfig::default())
        .unwrap();
    assert!(pump_until(
        &mut client,
        &mut state,
        Duration::from_secs(3),
        |s| { s.is_connected() }
    ));

    for sequence in 1..=10 {
        assert!(client.send_input_message(InputMessage {
            client_tick: sequence,
            sequence,
            movement: Vec2::RIGHT,
            actions: vec![],
        }));
        std::thread::sleep(Duration::from_millis(5));
        client.update(&mut state);
    }

    // The mock server acknowledges every processed input in its snapshots.
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let mut acked_all = false;
    while std::time::Instant::now() < deadline {
        client.update(&mut state);
        if client
            .effects
            .last_acked_sequence()
            .is_some_and(|acked| acked >= 10)
        {
            acked_all = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(acked_all, "server never acknowledged inputs up to 10");
}

#[test]
fn prediction_replays_on_top_of_authoritative_position() {
    // Pure unit-level integration of Predictor + InputBuffer, proving the
    // reconciliation contract without any networking.
    let mut predictor = Predictor::new(MovementModel {
        speed: 120.0,
        air_control: 1.0,
    });
    predictor.set_local_player(Some("p".into()));
    let mut buffer = InputBuffer::new(32);

    for sequence in 1..=4 {
        buffer.push(InputMessage {
            client_tick: 100 + sequence,
            sequence,
            movement: Vec2::RIGHT,
            actions: vec![],
        });
    }

    // Server processed the first two inputs and reports us at x = 4.
    let snapshot = lan_bash_client::protocol::message::Snapshot {
        tick: 102,
        acked_input_sequence: Some(2),
        players: vec![lan_bash_client::net::client::default_player_at(
            "p",
            Vec2::new(4.0, 0.0),
        )],
        ..Default::default()
    };
    predictor.on_snapshot(&snapshot, &mut buffer);

    assert_eq!(buffer.len(), 2, "acked inputs must be pruned");
    let prediction = predictor.predict(lan_bash_client::core::Tick(104), &buffer, 60);
    // Speed 120 / 60 Hz = 2 units per tick, two replayed inputs.
    assert!((prediction.position.x - 8.0).abs() < 1e-4);
}

#[test]
fn mock_driver_stops_cleanly_on_disconnect() {
    let mut client = GameClient::new(NetConfig::default());
    let mut state = GameState::new();
    client
        .connect_mock(Hello::new("bye", None), MockServerConfig::default())
        .unwrap();
    assert!(pump_until(
        &mut client,
        &mut state,
        Duration::from_secs(3),
        |s| { s.is_connected() }
    ));

    client.disconnect();
    assert_eq!(client.status(), ConnectionStatus::Disconnected);
}

#[test]
fn headless_loop_keeps_an_explicit_mock_connection() {
    // Regression: `run_headless` used to call `connect()` unconditionally and
    // silently replace an explicitly configured (mock) session.
    //
    // No runtime is provided on purpose: `run_headless` waits for the session
    // (`wait_for_session`) and must therefore bring its *own* runtime –
    // otherwise "playing without the server" is exactly as broken as this test
    // used to be under plain `cargo test`. It is also what `main` does.
    let mut app = ClientApp::new(ClientConfig::default());
    app.connect_mock(MockServerConfig {
        // Fast snapshots so a short headless run is deterministic.
        snapshot_interval_ms: 10,
        ..Default::default()
    });
    // ~120 ticks at 60 Hz ≈ 2 s of simulated time. (`run_headless` waits for
    // the handshake itself, so the mock server's first snapshot at ~10 ms is
    // always in.)
    app.run_headless(Some(120)).unwrap();

    assert!(app.state().is_connected(), "mock connection was dropped");
    assert_eq!(app.state().phase, Phase::Running);
    assert!(
        !app.state().players.is_empty(),
        "snapshots never reached the headless loop"
    );
}

/// Regression for the "input buffer full" flood in the GUI client.
///
/// Root cause was not the sampling rate: `Predictor::on_snapshot` (the only
/// place acknowledged inputs are pruned, and the only source of prediction
/// authority) was never called outside unit tests. The replay buffer therefore
/// only ever grew, and the local player was never predicted.
#[test]
fn acknowledged_inputs_are_pruned_while_playing() {
    let runtime = lan_bash_client::app::runtime::build_tokio_runtime().unwrap();
    let _enter = runtime.enter();

    let mut app = ClientApp::new(ClientConfig::default());
    app.connect_mock(MockServerConfig {
        snapshot_interval_ms: 20,
        ..Default::default()
    });
    // ~3 s at 60 Hz: long enough for hundreds of samples to be acked.
    app.run_headless(Some(180)).unwrap();

    assert!(app.state().is_connected());
    assert!(
        app.state().acked_input_sequence.unwrap_or(0) > 0,
        "server never acknowledged any input"
    );
    assert!(
        app.pending_inputs() < 30,
        "replay buffer is not pruned by acks: {} pending",
        app.pending_inputs()
    );
    assert_eq!(
        app.state().buffer_overflows,
        0,
        "acknowledged inputs should never overflow the buffer"
    );
    assert!(
        app.state().local_override.is_some(),
        "no prediction authority: snapshots never reached the predictor"
    );
}
