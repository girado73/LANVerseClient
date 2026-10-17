//! The game-thread half of the networking layer.
//!
//! [`NetClientHandle`] is a cheap, `Clone`+`Send` handle. The game loop uses it
//! to
//! * send already-encoded protocol messages (`send_hello`, `send_input`, …)
//! * drain inbound [`NetEvent`]s (`drain_events`)
//!
//! It never blocks and never touches the socket: both directions go through
//! bounded `tokio::mpsc` channels consumed by [`crate::net::session`].

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Mutex, mpsc};

use crate::core::Vec2;
use crate::input::InputActions;
use crate::net::events::NetEvent;
use crate::net::latency::LatencyEstimator;
use crate::protocol::PROTOCOL_VERSION;
use crate::protocol::message::{Hello, InputMessage, Ping, ProtocolMessage};

/// Lifecycle of the link, independent of *why* it ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionStatus {
    /// Not started yet.
    Disconnected,
    /// TCP/WebSocket handshake in flight.
    Connecting,
    /// Socket open, `hello` sent, waiting for `welcome`.
    Handshaking,
    /// Ready: inputs may be sent, snapshots are flowing.
    Connected,
    /// Backoff delay before the next attempt (`attempt` of `max`).
    Reconnecting { attempt: u32 },
    /// Terminal – either the server rejected us fatally or retries are exhausted.
    Failed,
}

impl ConnectionStatus {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Disconnected => "disconnected",
            Self::Connecting => "connecting",
            Self::Handshaking => "handshaking",
            Self::Connected => "connected",
            Self::Reconnecting { .. } => "reconnecting",
            Self::Failed => "failed",
        }
    }

    /// Whether gameplay input may currently be sent.
    pub fn can_send_gameplay(&self) -> bool {
        matches!(self, Self::Connected)
    }
}

#[derive(Debug)]
pub(crate) enum Command {
    Hello(Hello),
    Input(InputMessage),
    Ping(Ping),
    Raw(ProtocolMessage),
    Shutdown,
}

/// Handle for the game loop.
#[derive(Debug, Clone)]
pub struct NetClientHandle {
    commands: mpsc::UnboundedSender<Command>,
    events: std::sync::Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<NetEvent>>>,
}

impl NetClientHandle {
    /// Creates a handle plus the receiving ends the session driver needs.
    pub(crate) fn channel_pair() -> (
        Self,
        mpsc::UnboundedReceiver<Command>,
        mpsc::UnboundedSender<NetEvent>,
    ) {
        let (command_tx, command_rx) = mpsc::unbounded_channel();
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let handle = Self {
            commands: command_tx,
            events: Arc::new(Mutex::new(event_rx)),
        };
        (handle, command_rx, event_tx)
    }

    pub fn send_hello(&self, hello: Hello) -> Result<(), NetClientError> {
        self.send_command(Command::Hello(hello))
    }

    /// Sends one input sample. Called once per fixed tick while connected.
    pub fn send_input(&self, input: InputMessage) -> Result<(), NetClientError> {
        self.send_command(Command::Input(input))
    }

    /// Convenience wrapper building an input message from raw intent.
    pub fn send_intent(
        &self,
        client_tick: u64,
        sequence: u64,
        movement: Vec2,
        actions: Vec<InputActions>,
    ) -> Result<(), NetClientError> {
        self.send_input(InputMessage {
            client_tick,
            sequence,
            movement,
            actions,
        })
    }

    pub fn send_ping(&self) -> Result<(), NetClientError> {
        self.send_command(Command::Ping(Ping {
            client_time_ms: LatencyEstimator::now_ms(),
        }))
    }

    pub fn send_raw(&self, message: ProtocolMessage) -> Result<(), NetClientError> {
        self.send_command(Command::Raw(message))
    }

    /// Requests a clean close (no reconnect) and stops the driver.
    pub fn shutdown(&self) -> Result<(), NetClientError> {
        self.send_command(Command::Shutdown)
    }

    /// Returns every event received since the last call. Never blocks.
    pub fn drain_events(&self) -> Vec<NetEvent> {
        let mut guard = match self.events.try_lock() {
            Ok(guard) => guard,
            // Another caller is draining right now – that is fine, they got them.
            Err(_) => return Vec::new(),
        };
        let mut events = Vec::new();
        while let Ok(event) = guard.try_recv() {
            events.push(event);
        }
        events
    }

    fn send_command(&self, command: Command) -> Result<(), NetClientError> {
        self.commands
            .send(command)
            .map_err(|_| NetClientError::Disconnected)
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum NetClientError {
    #[error("network driver is not running")]
    Disconnected,
}

/// Live connection phase kept by the session driver.
///
/// This is a *state machine helper* – it contains no I/O so it can be unit
/// tested. The session driver calls the transition methods, which return the
/// [`ConnectionStatus`] that should be reported to the game loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lifecycle {
    /// Waiting before the `attempt`-th connection try (0 = immediate first try).
    WaitingToConnect {
        attempt: u32,
    },
    Connecting,
    Handshaking,
    Connected,
    /// Permanent stop.
    Terminated,
}

impl Default for Lifecycle {
    fn default() -> Self {
        Self::WaitingToConnect { attempt: 0 }
    }
}

impl Lifecycle {
    /// Called when the socket is open and `hello` is being sent.
    pub fn on_socket_open(&mut self) -> ConnectionStatus {
        *self = Self::Handshaking;
        ConnectionStatus::Handshaking
    }

    /// Called when a valid `welcome` arrived.
    pub fn on_welcome(&mut self) -> ConnectionStatus {
        *self = Self::Connected;
        ConnectionStatus::Connected
    }

    /// Called when the link dropped or a fatal error occurred.
    ///
    /// `reconnect_delay` is `None` when retries are disabled or exhausted.
    pub fn on_disconnect(
        &mut self,
        attempt: u32,
        reconnect_delay: Option<Duration>,
    ) -> ConnectionStatus {
        match reconnect_delay {
            Some(_) => {
                *self = Self::WaitingToConnect {
                    attempt: attempt + 1,
                };
                ConnectionStatus::Reconnecting {
                    attempt: attempt + 1,
                }
            }
            None => {
                *self = Self::Terminated;
                ConnectionStatus::Failed
            }
        }
    }

    pub fn terminate(&mut self) {
        *self = Self::Terminated;
    }

    pub fn is_terminated(&self) -> bool {
        matches!(self, Self::Terminated)
    }

    pub fn attempt(&self) -> u32 {
        match self {
            Self::WaitingToConnect { attempt } => *attempt,
            _ => 0,
        }
    }
}

/// Version the client speaks – mirrored in the hello message and validated
/// against the server's welcome.
pub const CLIENT_PROTOCOL_VERSION: u32 = PROTOCOL_VERSION;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_reconnects_then_fails() {
        let mut lifecycle = Lifecycle::default();
        assert_eq!(lifecycle.on_socket_open(), ConnectionStatus::Handshaking);
        assert_eq!(lifecycle.on_welcome(), ConnectionStatus::Connected);
        assert_eq!(
            lifecycle.on_disconnect(0, Some(Duration::from_millis(500))),
            ConnectionStatus::Reconnecting { attempt: 1 }
        );
        assert_eq!(lifecycle.on_disconnect(3, None), ConnectionStatus::Failed);
        assert!(lifecycle.is_terminated());
    }

    #[test]
    fn handle_drain_is_empty_after_driver_gone() {
        let (handle, _commands, events) = NetClientHandle::channel_pair();
        events.send(NetEvent::TransportError("x".into())).unwrap();
        drop(events);
        let drained = handle.drain_events();
        assert_eq!(drained.len(), 1);
        assert!(handle.drain_events().is_empty());
    }

    #[test]
    fn sending_after_driver_gone_is_an_error() {
        let (handle, commands, _events) = NetClientHandle::channel_pair();
        drop(commands);
        assert_eq!(handle.send_ping(), Err(NetClientError::Disconnected));
    }
}
