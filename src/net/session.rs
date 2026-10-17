//! Async session driver.
//!
//! One thread runs a small Tokio runtime. It owns the socket and the
//! command/event channels and performs:
//!
//! 1. **connect** (with exponential backoff on failure)
//! 2. **handshake** – send `hello`, wait for `welcome` (socket messages that
//!    arrive before `welcome` are forwarded; gameplay input is queued and sent
//!    as soon as the session is ready)
//! 3. **run** – `select!` over
//!    * outbound commands (`input`, `ping`, `shutdown` …)
//!    * inbound server messages → [`crate::net::events::classify`]
//!    * a periodic client-side ping
//!    * the heartbeat watchdog (no message for `heartbeat_timeout` ⇒ dead link)
//! 4. **reconnect** or terminate.
//!
//! Because gameplay inputs are *fire and forget* (latest input wins on the
//! server anyway), queued inputs are dropped on reconnect instead of being
//! replayed with stale ticks.

use std::future::Future;
use std::time::Duration;

use tokio::sync::mpsc;

use crate::config::ConnectionConfig;
use crate::net::connection::{
    CLIENT_PROTOCOL_VERSION, Command, ConnectionStatus, Lifecycle, NetClientHandle,
};
use crate::net::events::{NetEvent, classify};
use crate::net::latency::LatencyEstimator;
use crate::net::transport::{MessageTransport, WsTransport};
use crate::protocol::message::{Hello, ProtocolMessage, ServerError};

/// Interval between client-initiated latency probes.
const PING_INTERVAL: Duration = Duration::from_secs(2);
/// How often the driver checks the heartbeat watchdog.
const WATCHDOG_INTERVAL: Duration = Duration::from_millis(500);

/// Starts the driver on a dedicated thread.
///
/// The returned handle is `Clone`; the driver stops as soon as every clone is
/// dropped or [`NetClientHandle::shutdown`] is called.
pub fn spawn(config: ConnectionConfig, hello: Hello) -> anyhow::Result<NetClientHandle> {
    let url = config.url.clone();
    // The transport future is created lazily *inside* the driver's select loop,
    // so connecting does not block the heartbeat/ping timers.
    spawn_with(config, hello, move || {
        let url = url.clone();
        async move { Ok(WsTransport::connect_url(url).await?) }
    })
}

/// Starts the driver with a custom transport constructor.
///
/// `connect` is called once per connection attempt; returning `Err` is treated
/// like a failed attempt and triggers the reconnect policy. This is the seam
/// that lets tests and `--mock-server` swap in [`crate::net::mock::MockTransport`].
pub fn spawn_with<T, F, Fut>(
    config: ConnectionConfig,
    hello: Hello,
    connect: F,
) -> anyhow::Result<NetClientHandle>
where
    T: MessageTransport + 'static,
    F: FnMut() -> Fut + Send + 'static,
    Fut: Future<Output = anyhow::Result<T>> + Send + 'static,
{
    let (handle, command_rx, event_tx) = NetClientHandle::channel_pair();

    std::thread::Builder::new()
        .name("lan-bash-net".to_owned())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(err) => {
                    let _ = event_tx.send(NetEvent::TransportError(format!(
                        "failed to start network runtime: {err}"
                    )));
                    return;
                }
            };
            runtime.block_on(run(config, hello, connect, command_rx, event_tx));
            tracing::debug!("network driver stopped");
        })?;

    Ok(handle)
}

/// Why a connection attempt ended.
enum End {
    /// Clean stop requested by the game loop.
    Shutdown,
    /// Link failure – reconnect according to policy.
    Disconnected(String),
    /// Fatal server rejection – do not reconnect.
    Fatal(String),
}

async fn run<T, F, Fut>(
    config: ConnectionConfig,
    hello: Hello,
    mut connect: F,
    mut commands: mpsc::UnboundedReceiver<Command>,
    events: mpsc::UnboundedSender<NetEvent>,
) where
    T: MessageTransport,
    F: FnMut() -> Fut,
    Fut: Future<Output = anyhow::Result<T>>,
{
    let mut lifecycle = Lifecycle::default();
    let mut backoff_delay: Option<Duration> = None;

    loop {
        if let Some(delay) = backoff_delay.take() {
            tracing::info!(?delay, "waiting before reconnect");
            tokio::time::sleep(delay).await;
        }

        let _ = events.send(NetEvent::Status(ConnectionStatus::Connecting));
        let end = connect_once(
            &config,
            &hello,
            &mut connect,
            &mut commands,
            &events,
            &mut lifecycle,
        )
        .await;

        match end {
            End::Shutdown => {
                lifecycle.terminate();
                let _ = events.send(NetEvent::Status(ConnectionStatus::Disconnected));
                return;
            }
            End::Fatal(reason) => {
                tracing::warn!(%reason, "fatal connection error, not reconnecting");
                lifecycle.terminate();
                let _ = events.send(NetEvent::TransportError(reason));
                let _ = events.send(NetEvent::Status(ConnectionStatus::Failed));
                return;
            }
            End::Disconnected(reason) => {
                tracing::warn!(%reason, "connection lost");
                let _ = events.send(NetEvent::TransportError(reason));

                let attempt = lifecycle.attempt();
                let reconnect_delay = config.reconnect.enabled.then(|| {
                    if config.reconnect.max_attempts > 0 && attempt >= config.reconnect.max_attempts
                    {
                        None
                    } else {
                        Some(config.reconnect.delay_for(attempt + 1))
                    }
                });
                let reconnect_delay = reconnect_delay.flatten();

                let status = lifecycle.on_disconnect(attempt, reconnect_delay);
                let _ = events.send(NetEvent::Status(status));
                if reconnect_delay.is_none() {
                    return;
                }
                backoff_delay = reconnect_delay;
            }
        }
    }
}

async fn connect_once<T, F, Fut>(
    config: &ConnectionConfig,
    hello: &Hello,
    connect: &mut F,
    commands: &mut mpsc::UnboundedReceiver<Command>,
    events: &mpsc::UnboundedSender<NetEvent>,
    lifecycle: &mut Lifecycle,
) -> End
where
    T: MessageTransport,
    F: FnMut() -> Fut,
    Fut: Future<Output = anyhow::Result<T>>,
{
    // The connect future lives in the select loop so a hanging handshake can
    // neither block pings nor be interrupted by commands.
    let mut connecting = std::pin::pin!(connect());
    let mut transport = tokio::select! {
        result = &mut connecting => match result {
            Ok(transport) => transport,
            Err(err) => return End::Disconnected(err.to_string()),
        },
        command = commands.recv() => {
            // Only shutdown makes sense before a transport exists.
            return match command {
                Some(Command::Shutdown) | None => End::Shutdown,
                Some(_) => End::Disconnected("connection attempt aborted".into()),
            };
        }
    };

    // --- handshake -------------------------------------------------------
    let _ = events.send(NetEvent::Status(lifecycle.on_socket_open()));

    // Collect gameplay inputs that were sampled before the link was ready and
    // discard them – their ticks are meaningless now. Only the hello survives
    // the handshake, and the game loop re-sends it if needed, so we simply
    // drain pending commands except an explicit shutdown.
    let mut pending_inputs = 0usize;
    while let Ok(command) = commands.try_recv() {
        match command {
            Command::Shutdown => {
                let _ = transport.close().await;
                return End::Shutdown;
            }
            Command::Input(_) => pending_inputs += 1,
            _ => {}
        }
    }
    if pending_inputs > 0 {
        tracing::debug!(pending_inputs, "dropped inputs sampled before handshake");
    }

    if let Err(err) = send_message(&mut transport, &ProtocolMessage::Hello(hello.clone())).await {
        return End::Disconnected(err);
    }

    // --- event loop ------------------------------------------------------
    let mut latency = LatencyEstimator::default();
    let mut ping_interval = tokio::time::interval(PING_INTERVAL);
    ping_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut watchdog = tokio::time::interval(WATCHDOG_INTERVAL);
    watchdog.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_received = tokio::time::Instant::now();
    let mut handshake_complete = false;

    loop {
        tokio::select! {
            biased;

            command = commands.recv() => {
                let Some(command) = command else {
                    let _ = transport.close().await;
                    return End::Shutdown;
                };

                match command {
                    Command::Shutdown => {
                        let _ = transport.close().await;
                        return End::Shutdown;
                    }
                    Command::Hello(hello) => {
                        if let Err(err) = send_message(&mut transport, &ProtocolMessage::Hello(hello)).await {
                            return End::Disconnected(err);
                        }
                    }
                    Command::Input(input) => {
                        // Inputs before the welcome are stale – the server has no
                        // idea who we are yet.
                        if handshake_complete
                            && let Err(err) = send_message(&mut transport, &ProtocolMessage::Input(input)).await {
                                return End::Disconnected(err);
                            }
                    }
                    Command::Ping(ping) => {
                        if let Err(err) = send_message(&mut transport, &ProtocolMessage::Ping(ping)).await {
                            return End::Disconnected(err);
                        }
                    }
                    Command::Raw(message) => {
                        if let Err(err) = send_message(&mut transport, &message).await {
                            return End::Disconnected(err);
                        }
                    }
                }
            }

            message = transport.next_message() => {
                last_received = tokio::time::Instant::now();
                let text = match message {
                    Ok(Some(text)) => text,
                    Ok(None) => return End::Disconnected("server closed the connection".into()),
                    Err(err) => return End::Disconnected(err.to_string()),
                };

                let parsed = match ProtocolMessage::from_json_str(&text) {
                    Ok(parsed) => parsed,
                    Err(err) => {
                        // One malformed frame must not kill the session, but it
                        // is a protocol bug worth surfacing.
                        tracing::warn!(%err, frame = %truncate(&text), "dropping malformed server message");
                        let _ = events.send(NetEvent::TransportError(format!("malformed frame: {err}")));
                        continue;
                    }
                };

                // The welcome is the only message allowed to promote us to Connected.
                if let ProtocolMessage::Welcome(welcome) = &parsed
                    && !handshake_complete
                {
                    if welcome.protocol_version != CLIENT_PROTOCOL_VERSION {
                        return End::Fatal(format!(
                            "protocol mismatch: server {} vs client {CLIENT_PROTOCOL_VERSION}",
                            welcome.protocol_version
                        ));
                    }
                    handshake_complete = true;
                    ping_interval.reset();
                    let _ = events.send(NetEvent::Status(lifecycle.on_welcome()));
                }

                for event in classify(parsed, &mut latency) {
                    let fatal_error = matches!(&event, NetEvent::ServerError(ServerError { fatal: true, .. }));
                    let fatal_reason = match &event {
                        NetEvent::ServerError(error) if error.fatal => {
                            Some(format!("{}: {}", error.code, error.message))
                        }
                        _ => None,
                    };
                    let _ = events.send(event);
                    if fatal_error {
                        return End::Fatal(fatal_reason.unwrap_or_else(|| "server error".into()));
                    }
                }
            }

            _ = ping_interval.tick() => {
                let ping = ProtocolMessage::Ping(crate::protocol::message::Ping {
                    client_time_ms: LatencyEstimator::now_ms(),
                });
                if let Err(err) = send_message(&mut transport, &ping).await {
                    return End::Disconnected(err);
                }
            }

            _ = watchdog.tick() => {
                if last_received.elapsed() > config.heartbeat_timeout {
                    return End::Disconnected(format!(
                        "heartbeat timeout after {:?} without server messages",
                        config.heartbeat_timeout
                    ));
                }
            }
        }
    }
}

async fn send_message(
    transport: &mut impl MessageTransport,
    message: &ProtocolMessage,
) -> Result<(), String> {
    let payload = message
        .to_json_string()
        .map_err(|err| format!("failed to encode {}: {err}", message.type_name()))?;
    tracing::trace!(
        kind = message.type_name(),
        bytes = payload.len(),
        "→ server"
    );
    transport.send(payload).await.map_err(|err| err.to_string())
}

fn truncate(text: &str) -> &str {
    const MAX: usize = 200;
    match text.char_indices().nth(MAX) {
        Some((index, _)) => &text[..index],
        None => text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_is_char_boundary_safe() {
        let text = "ä".repeat(500);
        let cut = truncate(&text);
        assert_eq!(cut.chars().count(), 200);
    }

    #[test]
    fn handshake_hello_serializes() {
        let hello = Hello::new("tester", Some("secret".into()));
        let msg = ProtocolMessage::Hello(hello);
        let json = msg.to_json_string().unwrap();
        assert!(json.contains("\"type\":\"hello\""));
        assert!(json.contains("\"token\":\"secret\""));
    }
}
