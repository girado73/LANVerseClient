//! "Start the client first, the server second" – the normal LAN order.
//!
//! The reported symptom was a client that logged `Connection refused` every
//! second forever while filling its input buffer, and never connected once the
//! server did come up. This test pins the behaviour down end-to-end with the
//! real WebSocket transport: retry while nobody listens, then connect and
//! play as soon as a server answers.

use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use lan_bash_client::config::{ConnectionConfig, NetConfig, ReconnectPolicy};
use lan_bash_client::models::GameState;
use lan_bash_client::net::client::GameClient;
use lan_bash_client::net::connection::ConnectionStatus;
use lan_bash_client::protocol::message::Hello;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

fn pump(
    client: &mut GameClient,
    state: &mut GameState,
    timeout: Duration,
    done: impl Fn(&GameState) -> bool,
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

/// Minimal `game:serve`: answers `hello` with `welcome`, `ping` with `pong`
/// and pushes one snapshot with a single player.
async fn serve_game(port: u16) {
    let listener = TcpListener::bind(("127.0.0.1", port)).await.unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await else {
                    return;
                };
                let mut welcomed = false;
                let mut snapshot = tokio::time::interval(Duration::from_millis(20));
                let mut tick = 0u64;
                loop {
                    tokio::select! {
                        message = ws.next() => {
                            let Some(Ok(Message::Text(text))) = message else { return };
                            if text.contains("\"hello\"") && !welcomed {
                                welcomed = true;
                                let _ = ws.send(Message::Text(
                                    r#"{"type":"welcome","protocol_version":1,"player_id":"p-9",
                                        "match_id":"lan-late","tick_rate":60,"server_tick":0}"#
                                        .into(),
                                )).await;
                                let _ = ws.send(Message::Text(
                                    r#"{"type":"round_state","phase":"running","round":"lan-late",
                                        "round_number":1,"countdown_ms":0}"#
                                        .into(),
                                )).await;
                            } else if text.contains("\"ping\"") {
                                let start = text.split("\"client_time_ms\":").nth(1)
                                    .and_then(|rest| rest.trim_start().split(|c: char| !c.is_ascii_digit()).next())
                                    .unwrap_or("0").to_owned();
                                let pong = format!(r#"{{"type":"pong","client_time_ms":{start}}}"#);
                                let _ = ws.send(Message::Text(pong.into())).await;
                            }
                        }
                        _ = snapshot.tick() => {
                            if !welcomed {
                                continue;
                            }
                            tick += 1;
                            let frame = format!(
                                r#"{{"type":"snapshot","tick":{tick},"acked_input_sequence":0,
                                    "round":{{"round_id":"lan-late","time_left_ms":179000}},
                                    "players":[{{"id":"p-9","name":"Player","position":{{"x":1600,"y":900}},
                                    "velocity":{{"x":0,"y":0}},"facing":{{"x":1,"y":0}},"hp":100,
                                    "max_hp":100,"is_alive":true,"score":0,"team":null}}]}}"#
                            );
                            if ws.send(Message::Text(frame.into())).await.is_err() {
                                return;
                            }
                        }
                    }
                }
            });
        }
    });
}

#[test]
fn client_started_before_the_server_recovers_once_it_starts() {
    let runtime = lan_bash_client::app::runtime::build_tokio_runtime().unwrap();
    let _guard = runtime.enter();

    // Reserve a port and keep the listener *closed* until the client has been
    // running for a while.
    let port = {
        let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        probe.local_addr().unwrap().port()
    };

    let mut client = GameClient::new(NetConfig {
        connection: ConnectionConfig {
            url: format!("ws://127.0.0.1:{port}/app/testkey"),
            reconnect: ReconnectPolicy {
                base_delay: Duration::from_millis(50),
                max_delay: Duration::from_millis(100),
                ..Default::default()
            },
            ..Default::default()
        },
        ..Default::default()
    });
    let mut state = GameState::new();
    client.connect(Hello::new("Player", None)).unwrap();

    // 500 ms of "server is not running": the client must survive it and keep
    // retrying (it used to run into "retries exhausted" and give up).
    std::thread::sleep(Duration::from_millis(500));
    client.update(&mut state);
    assert!(
        !state.connection.can_send_gameplay(),
        "must not be connected before the server starts"
    );
    assert_ne!(
        state.connection,
        ConnectionStatus::Failed,
        "a missing server is not a fatal error"
    );

    // Server starts now.
    runtime.spawn(serve_game(port));

    assert!(
        pump(&mut client, &mut state, Duration::from_secs(5), |s| s
            .is_connected()),
        "client never reconnected after the server started: {:?}",
        state.connection
    );
    assert_eq!(state.local_player.as_ref().map(|p| p.as_str()), Some("p-9"));
    assert_eq!(
        state.match_id.as_ref().map(|m| m.as_str()),
        Some("lan-late")
    );

    assert!(
        pump(&mut client, &mut state, Duration::from_secs(3), |s| s
            .player(&"p-9".into())
            .is_some()),
        "no player state arrived over the real transport"
    );
    // The round timer comes from the snapshot (and is not clobbered by the
    // `round_state` frame, which sends `countdown_ms: 0`).
    assert!(state.round.time_left_ms > 0, "round timer was lost");
}
