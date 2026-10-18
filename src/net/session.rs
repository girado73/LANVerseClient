//! Async session driver.
//!
//! One thread runs a small Tokio runtime. It owns the socket and the
//! command/event channels and performs:
//!
//! 1. **connect** (with backoff on failure, bounded by `connect_timeout`)
//! 2. **handshake** – send `hello`, wait for `welcome` (socket messages that
//!    arrive before `welcome` are forwarded; `welcome` itself is reported
//!    *before* the `Connected` status so the game loop never sees a session
//!    without an identity)
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
    spawn_in(driver_runtime()?, config, hello, {
        Box::new(move || {
            let url = url.clone();
            Box::pin(async move {
                let transport: Box<dyn MessageTransport> =
                    Box::new(WsTransport::connect_url(url).await?);
                Ok(transport)
            }) as BoxFuture
        })
    })
}

/// A runtime for the connection future's setup (`MockTransport::connect`).
///
/// Only needed when the caller has no runtime of its own. A **current-thread**
/// runtime is deliberate: with no worker threads, its destructor never blocks,
/// so it can live and die inside a `block_on` (a multi-thread runtime's
/// destructor waits for its workers and panics in that situation).
pub fn driver_runtime() -> anyhow::Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .thread_name("lan-bash-driver")
        .build()?)
}

/// How the driver creates a (new) transport for one connection attempt.
///
/// Boxed because the concrete transport differs between the WebSocket and the
/// in-memory mock; the driver itself only needs [`MessageTransport`].
pub type ConnectFn = Box<dyn FnMut() -> BoxFuture + Send>;
/// One in-flight connection attempt.
pub type BoxFuture =
    std::pin::Pin<Box<dyn Future<Output = anyhow::Result<Box<dyn MessageTransport>>> + Send>>;

/// Starts the driver with a custom transport constructor (generic flavour).
///
/// `connect` is called once per connection attempt; returning `Err` is treated
/// like a failed attempt and triggers the reconnect policy. Use
/// [`spawn_with_transports`] for a transport that is only known at runtime
/// (the in-memory mock, or the boxed WebSocket factory `GameClient` uses).
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
    let mut connect = Some(connect);
    spawn_with_transports(
        config,
        hello,
        Box::new(move || {
            // `connect()` is called lazily (on the first poll, inside the
            // factory runtime) because transports spawn tasks while being
            // created. The closure itself may run without a runtime.
            let connect = connect.take();
            Box::pin(async move {
                let mut connect =
                    connect.ok_or_else(|| anyhow::anyhow!("transport factory called twice"))?;
                let transport: Box<dyn MessageTransport> = Box::new(connect().await?);
                Ok(transport)
            }) as BoxFuture
        }),
    )
}

/// [`spawn_with`] for an already type-erased transport factory.
///
/// The factory future is polled inside a runtime provided by the driver, and so
/// is the setup it does when it first runs (e.g. spawning the mock server).
pub fn spawn_with_transports(
    config: ConnectionConfig,
    hello: Hello,
    connect: ConnectFn,
) -> anyhow::Result<NetClientHandle> {
    spawn_in(driver_runtime()?, config, hello, connect)
}

/// [`spawn_with`], but connected through an explicitly provided factory.
///
/// The driver *thread* always gets its own current-thread runtime (it must not
/// share one with the game loop). The `factory` runtime is *polled* by the
/// driver loop: the connection future (`WsTransport::connect_url`, the mock
/// transport) needs a reactor. It is shut down cleanly when the driver stops.
pub fn spawn_in(
    factory: tokio::runtime::Runtime,
    config: ConnectionConfig,
    hello: Hello,
    mut connect: ConnectFn,
) -> anyhow::Result<NetClientHandle> {
    let (handle, command_rx, event_tx) = NetClientHandle::channel_pair();

    std::thread::Builder::new()
        .name("lan-bash-net".to_owned())
        .spawn(move || {
            // A current-thread runtime is destroyed while `run` is still on
            // the stack, but it has no worker threads to wait for, so its
            // destructor cannot block ("cannot drop a runtime ...").
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
            // The factory runtime lives on this thread for the whole session;
            // it is only ever used by `enter`/`block_on`, never `enter`ed
            // around async work, so it can coexist with the driver runtime.
            runtime.block_on(run(
                &factory,
                config,
                hello,
                &mut connect,
                command_rx,
                event_tx,
            ));
            // Dropping a runtime inside `block_on` panics ("cannot drop a
            // runtime where blocking is not allowed", which then kills the
            // driver's blocking pool). Shut it down explicitly, outside.
            factory.shutdown_timeout(std::time::Duration::from_millis(100));
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

async fn run(
    factory: &tokio::runtime::Runtime,
    config: ConnectionConfig,
    hello: Hello,
    connect: &mut ConnectFn,
    mut commands: mpsc::UnboundedReceiver<Command>,
    events: mpsc::UnboundedSender<NetEvent>,
) {
    let mut lifecycle = Lifecycle::default();
    let mut backoff_delay: Option<Duration> = None;

    loop {
        if let Some(delay) = backoff_delay.take() {
            tracing::debug!(?delay, "waiting before reconnect");
            tokio::time::sleep(delay).await;
        }

        let _ = events.send(NetEvent::Status(ConnectionStatus::Connecting));
        // The connection future must be *created* and *polled* inside a runtime
        // (its I/O driver lives there): `MockTransport::connect` spawns the
        // fake server, the WebSocket client registers with the reactor.
        // `enter` makes `factory` the ambient runtime while the future is
        // created; polling happens in `connect_once` on the driver's runtime,
        // i.e. inside *a* runtime, which is all that matters. A caller that is
        // already inside a runtime is fine – `enter` nests.
        let mut connecting = {
            let _guard = factory.enter();
            connect()
        };
        let end = connect_once(
            &config,
            &hello,
            &mut connecting,
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
                // A server that is simply not running yet is the normal case
                // during LAN setup: log it loudly once per outage, then at debug
                // level, otherwise "connection lost" fills the log forever.
                let first_failure = lifecycle.attempt() == 0;
                if first_failure {
                    tracing::warn!(%reason, url = %config.url, "connection lost");
                } else {
                    tracing::debug!(%reason, "connection attempt failed");
                }
                let _ = events.send(NetEvent::TransportError(reason));

                // The `attempt`-th retry is the one about to start.
                let attempt = lifecycle.attempt();
                let next_attempt = attempt + 1;
                let retry = config.reconnect.enabled
                    && (config.reconnect.max_attempts == 0
                        || next_attempt <= config.reconnect.max_attempts);
                let reconnect_delay = retry.then(|| config.reconnect.delay_for(next_attempt));
                let status = lifecycle.on_disconnect(attempt, reconnect_delay);
                let _ = events.send(NetEvent::Status(status));

                let Some(delay) = reconnect_delay else {
                    tracing::error!("giving up, reconnect retries exhausted");
                    return;
                };
                backoff_delay = Some(delay);
            }
        }
    }
}

async fn connect_once(
    config: &ConnectionConfig,
    hello: &Hello,
    connecting: &mut BoxFuture,
    commands: &mut mpsc::UnboundedReceiver<Command>,
    events: &mpsc::UnboundedSender<NetEvent>,
    lifecycle: &mut Lifecycle,
) -> End {
    // The connect future lives in the select loop so a hanging handshake can
    // neither block pings nor be interrupted by commands.
    let mut transport = tokio::select! {
        result = connecting.as_mut() => match result {
            Ok(transport) => transport,
            Err(err) => return End::Disconnected(err.to_string()),
        },
        // A TCP connect to a filtered/bridged address can hang for minutes;
        // without this the whole driver would sit in `Connecting` forever.
        _ = tokio::time::sleep(config.connect_timeout) => {
            return End::Disconnected(format!(
                "connect timed out after {:?}",
                config.connect_timeout
            ));
        }
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

    // Inputs sampled while the socket was being opened were never sent; the
    // game loop keeps sampling every tick, so only the newest would matter.
    // They are *skipped* here (see `Command::Input` below), not drained from
    // the receiver: draining would stress the channel for no benefit, the loop
    // simply drops them again in the main select.
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
        tracing::debug!(pending_inputs, "discarded inputs sampled before handshake");
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
    tracing::debug!(url = %config.url, "hello sent, waiting for welcome");

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

                let parsed = match crate::protocol::parse_frame(&text) {
                    Ok(parsed) => parsed,
                    Err(err) => {
                        // One malformed frame must not kill the session, and it
                        // must not wipe the HUD either (a bad frame is a server
                        // bug, not a connection problem) – log and carry on.
                        tracing::warn!(%err, frame = %truncate(&text), "dropping malformed server message");
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
                    // Report the welcome *first*: `GameClient` applies events in
                    // order, so `Connected` always observes a local player id.
                    let _ = events.send(NetEvent::Welcome(welcome.clone()));
                    let _ = events.send(NetEvent::Status(lifecycle.on_welcome()));
                    tracing::info!(
                        player = %welcome.player_id,
                        tick_rate = welcome.tick_rate,
                        "handshake complete"
                    );
                    continue;
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
                if !handshake_complete {
                    // The socket is up but nobody answered `hello`. Some
                    // server versions send `pusher:connection_established`
                    // and then nothing; without this the client would wait
                    // for the full heartbeat timeout (and show "handshaking")
                    // while the server is unreachable for the game protocol.
                    if last_received.elapsed() > config.handshake_timeout {
                        return End::Disconnected(format!(
                            "no welcome within {:?}",
                            config.handshake_timeout
                        ));
                    }
                    continue;
                }
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
