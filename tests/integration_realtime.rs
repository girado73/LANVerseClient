//! The real transport and session driver against a real WebSocket server.
//!
//! The mock server (`net::mock`) covers the protocol logic but not the byte
//! level: handshake, framing, timeouts and reconnect used to be untested. These
//! tests run the actual [`WsTransport`] and [`session`] driver against a
//! `tokio-tungstenite` server on `127.0.0.1:0`, speaking the exact payloads
//! Laravel's `game:serve` sends (see `docs/protocol.md`).
//!
//! Everything is bounded by real timeouts, so a regression shows up as a
//! failing assertion, never as a hanging test.

use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use lan_bash_client::config::{ConnectionConfig, NetConfig, ReconnectPolicy};
use lan_bash_client::net::client::GameClient;
use lan_bash_client::net::connection::{ConnectionStatus, NetClientHandle};
use lan_bash_client::net::session;
use lan_bash_client::protocol::message::Hello;
use lan_bash_client::protocol::{ProtocolMessage, parse_frame};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;

/// What the fake server should do after the client said `hello`.
#[derive(Clone)]
enum Behaviour {
    /// Send `welcome` + a running `round_state` + snapshots, like a healthy
    /// `game:serve`.
    Healthy,
    /// Accept the socket, send Reverb's `pusher:connection_established` and
    /// then never answer the game protocol.
    Silent,
}

///
/// The tests keep a Tokio runtime alive on the test thread *before* calling
/// `client.connect()`: the session driver creates the WebSocket on the calling
/// thread's runtime, so without one the connection silently never opens.
struct FakeServer {
    url: String,
    /// `player_name` values from `hello` frames received so far.
    hellos: mpsc::UnboundedReceiver<String>,
}

/// Starts a fake server on a random local port.
async fn start_server(behaviour: Behaviour) -> FakeServer {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let url = format!("ws://127.0.0.1:{port}/app/testkey");
    let (hello_tx, hello_rx) = mpsc::unbounded_channel();

    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let behaviour = behaviour.clone();
            let hello_tx = hello_tx.clone();
            tokio::spawn(async move {
                let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await else {
                    return;
                };
                // Reverb always greets first – the client must ignore it.
                let _ = ws
                    .send(Message::Text(
                        r#"{"event":"pusher:connection_established","data":"{\"socket_id\":\"1.2\"}"}"#
                            .into(),
                    ))
                    .await;

                let mut tick: u64 = 0;
                match behaviour {
                    Behaviour::Silent => {
                        // Swallow everything until the client gives up.
                        while let Some(Ok(_)) = ws.next().await {}
                    }
                    Behaviour::Healthy => {
                        let mut snapshot = tokio::time::interval(Duration::from_millis(20));
                        loop {
                            tokio::select! {
                                message = ws.next() => {
                                    let Some(Ok(Message::Text(text))) = message else { return; };
                                    let frame = match parse_frame(text.as_str()) {
                                        Ok(frame) => frame,
                                        Err(_) => continue,
                                    };
                                    match frame {
                                        ProtocolMessage::Hello(hello) => {
                                            let _ = hello_tx.send(hello.player_name);
                                            send(&mut ws, &ProtocolMessage::Welcome(welcome())).await;
                                            send(&mut ws, &ProtocolMessage::RoundState(
                                                lan_bash_client::protocol::message::RoundState {
                                                    phase: lan_bash_client::protocol::message::Phase::Running,
                                                    round: Some("lan-1".into()),
                                                    round_number: Some(1),
                                                    countdown_ms: None,
                                                    winner: None,
                                                },
                                            )).await;
                                        }
                                        ProtocolMessage::Ping(ping) => {
                                            send(&mut ws, &ProtocolMessage::Pong(
                                                lan_bash_client::protocol::message::Pong {
                                                    client_time_ms: ping.client_time_ms,
                                                    server_time_ms: None,
                                                },
                                            )).await;
                                        }
                                        ProtocolMessage::Input(input) => {
                                            // Reply with a snapshot that acks it.
                                            let snapshot = snapshot_json(tick.max(input.client_tick), input.sequence);
                                            let _ = ws.send(Message::Text(snapshot.into())).await;
                                        }
                                        _ => {}
                                    }
                                }
                                _ = snapshot.tick() => {
                                    tick += 1;
                                    let _ = ws.send(Message::Text(snapshot_json(tick, 0).into())).await;
                                }
                            }
                        }
                    }
                }
            });
        }
    });

    FakeServer {
        url,
        hellos: hello_rx,
    }
}

async fn send(
    ws: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    msg: &ProtocolMessage,
) {
    let _ = ws
        .send(Message::Text(msg.to_json_string().unwrap().into()))
        .await;
}

/// Verbatim shape of Laravel's `WelcomeMessage` (no world/move_speed yet).
fn welcome() -> lan_bash_client::protocol::message::Welcome {
    parse_frame(
        r#"{"type":"welcome","protocol_version":1,"player_id":"p-1","session_id":"s-1",
            "match_id":"lan-20261018","tick_rate":60,"server_tick":0,"message":"Welcome"}"#,
    )
    .map(|message| match message {
        ProtocolMessage::Welcome(welcome) => welcome,
        other => panic!("expected welcome, got {other:?}"),
    })
    .unwrap()
}

/// Verbatim shape of Laravel's `SnapshotMessage` (including `team: null`).
fn snapshot_json(tick: u64, acked: u64) -> String {
    format!(
        r#"{{"type":"snapshot","tick":{tick},"acked_input_sequence":{acked},
           "round":{{"round_id":"lan-20261018","time_left_ms":180000}},
           "players":[{{"id":"p-1","name":"Alice","position":{{"x":768.62,"y":630}},
           "velocity":{{"x":0,"y":0}},"facing":{{"x":1,"y":0}},"hp":100,"max_hp":100,
           "is_alive":true,"score":0,"team":null}}]}}"#
    )
}

fn connection(url: &str) -> ConnectionConfig {
    ConnectionConfig {
        url: url.to_owned(),
        reconnect: ReconnectPolicy {
            base_delay: Duration::from_millis(20),
            max_delay: Duration::from_millis(100),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn pump(
    client: &mut GameClient,
    state: &mut lan_bash_client::models::GameState,
    timeout: Duration,
    done: impl Fn(&lan_bash_client::models::GameState) -> bool,
) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        client.update(state);
        if done(state) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    false
}

/// The happy path used to be unverified end-to-end.
#[test]
fn real_websocket_handshake_and_snapshots() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let _guard = runtime.enter();

    let mut server = runtime.block_on(start_server(Behaviour::Healthy));
    let mut client = GameClient::new(NetConfig {
        connection: connection(&server.url),
        ..Default::default()
    });
    let mut state = lan_bash_client::models::GameState::new();
    client.connect(Hello::new("Alice", None)).unwrap();

    assert!(
        pump(&mut client, &mut state, Duration::from_secs(3), |s| s
            .is_connected()),
        "never connected over the real transport: {:?} / {:?}",
        state.connection,
        state.last_error
    );
    assert_eq!(state.local_player.as_ref().map(|p| p.as_str()), Some("p-1"));
    assert!(
        pump(&mut client, &mut state, Duration::from_secs(3), |s| !s
            .players
            .is_empty()),
        "no snapshot reached GameState"
    );
    assert_eq!(state.players.len(), 1);
    assert_eq!(
        state.match_id.as_ref().map(|m| m.as_str()),
        Some("lan-20261018")
    );
    // The Laravel world size is only known because the client defaults to it.
    assert_eq!(state.world.width, 3200.0);

    // The client must have answered with a hello carrying its name.
    let hellos = runtime.block_on(async { server.hellos.recv().await });
    assert_eq!(hellos.as_deref(), Some("Alice"));
}

/// Regression for the "pusher:connection_established and then nothing" mode:
/// the client must give up quickly instead of sitting in `Handshaking` until
/// the general heartbeat timeout.
#[test]
fn silent_server_times_out_the_handshake() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let _guard = runtime.enter();

    let server = runtime.block_on(start_server(Behaviour::Silent));
    let mut client = GameClient::new(NetConfig {
        connection: ConnectionConfig {
            handshake_timeout: Duration::from_millis(300),
            ..connection(&server.url)
        },
        ..Default::default()
    });
    let mut state = lan_bash_client::models::GameState::new();
    client.connect(Hello::new("Alice", None)).unwrap();

    let started = Instant::now();
    assert!(
        pump(&mut client, &mut state, Duration::from_secs(5), |s| {
            !s.connection.can_send_gameplay()
                && s.last_error
                    .as_deref()
                    .is_some_and(|e| e.contains("no welcome"))
        }),
        "handshake timeout was not reported, status {:?}",
        state.connection
    );
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "handshake timeout took {:?}",
        started.elapsed()
    );
}

/// A server that is not running yet is the normal LAN start-up order. The
/// driver must keep retrying and connect once the server appears.
#[test]
fn driver_reconnects_when_the_server_starts_later() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let _guard = runtime.enter();

    // Reserve a port, then release it so the first attempts are refused.
    let port = {
        let listener = runtime.block_on(TcpListener::bind("127.0.0.1:0")).unwrap();
        listener.local_addr().unwrap().port()
    };
    let url = format!("ws://127.0.0.1:{port}/app/testkey");

    let handle: NetClientHandle = session::spawn(
        ConnectionConfig {
            url: url.clone(),
            reconnect: ReconnectPolicy {
                base_delay: Duration::from_millis(20),
                max_delay: Duration::from_millis(50),
                ..Default::default()
            },
            ..Default::default()
        },
        Hello::new("late", None),
    )
    .unwrap();

    // First attempt fails: the transport error is about a refused connection.
    // Status has to be `Reconnecting`, never `Failed` (Retrying is fine when
    // attempts are exhausted, but the default policy retries forever).
    std::thread::sleep(Duration::from_millis(150));
    let mut saw_reconnect = false;
    for event in handle.drain_events() {
        match event {
            lan_bash_client::net::events::NetEvent::Status(status) => {
                assert_ne!(
                    status,
                    ConnectionStatus::Failed,
                    "connection refused must not be fatal"
                );
                if matches!(status, ConnectionStatus::Reconnecting { .. }) {
                    saw_reconnect = true;
                }
            }
            lan_bash_client::net::events::NetEvent::TransportError(error) => {
                assert!(
                    error.contains("Connection refused"),
                    "unexpected transport error: {error}"
                );
            }
            _ => {}
        }
    }
    assert!(saw_reconnect, "driver did not schedule a reconnect");

    // Now the server "starts".
    let server = runtime.block_on(start_server_on(port));
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut connected = false;
    while Instant::now() < deadline {
        for event in handle.drain_events() {
            if event == lan_bash_client::net::events::NetEvent::Status(ConnectionStatus::Connected)
            {
                connected = true;
            }
        }
        if connected {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        connected,
        "driver never reconnected after the server started"
    );
    let _ = handle.shutdown();
    drop(server);
}

/// Starts the healthy fake server on an already known port.
async fn start_server_on(port: u16) -> oneshot::Sender<()> {
    let listener = TcpListener::bind(("127.0.0.1", port)).await.unwrap();
    let (stop_tx, mut stop_rx) = oneshot::channel::<()>();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = &mut stop_rx => return,
                accepted = listener.accept() => {
                    let Ok((stream, _)) = accepted else { return };
                    tokio::spawn(async move {
                        let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await else { return };
                        while let Some(Ok(Message::Text(text))) = ws.next().await {
                            match parse_frame(text.as_str()) {
                                Ok(ProtocolMessage::Hello(_)) => {
                                    send(&mut ws, &ProtocolMessage::Welcome(welcome())).await;
                                }
                                Ok(ProtocolMessage::Ping(ping)) => {
                                    send(&mut ws, &ProtocolMessage::Pong(
                                        lan_bash_client::protocol::message::Pong {
                                            client_time_ms: ping.client_time_ms,
                                            server_time_ms: None,
                                        },
                                    )).await;
                                }
                                _ => {}
                            }
                        }
                    });
                }
            }
        }
    });
    stop_tx
}
