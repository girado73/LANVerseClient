//! Typed event stream from the networking layer to the game loop.
//!
//! The network driver never mutates game state directly. It converts server
//! messages into [`NetEvent`]s, the game loop drains them once per frame and
//! `net::client::GameClient` applies them. This guarantees deterministic
//! ordering relative to ticks, and that no lock is held while rendering.

use crate::net::connection::ConnectionStatus;
use crate::net::latency::LatencyEstimator;
use crate::protocol::message::{
    GameEvent, ProtocolMessage, RoundState, ServerError, Snapshot, Welcome,
};

#[derive(Debug, Clone, PartialEq)]
pub enum NetEvent {
    /// Connection state transition, for the HUD.
    Status(ConnectionStatus),
    /// Handshake accepted – we now have an identity on the server.
    Welcome(Welcome),
    /// Authoritative snapshot, already deserialized (validation happens in the client).
    Snapshot(Snapshot),
    /// One discrete gameplay event from the server.
    Game(GameEvent),
    /// Match phase transition.
    RoundState(RoundState),
    /// Measured RTT in milliseconds.
    Latency(f32),
    /// Error reported by the server (`fatal == true` also triggers a disconnect).
    ServerError(ServerError),
    /// Transport-level failure (will be followed by a reconnect if enabled).
    TransportError(String),
}

/// Maps protocol messages to [`NetEvent`]s.
///
/// Used by both the real session driver and the mock server so the two paths
/// cannot drift apart.
pub fn classify(message: ProtocolMessage, latency: &mut LatencyEstimator) -> Vec<NetEvent> {
    match message {
        ProtocolMessage::Welcome(welcome) => vec![NetEvent::Welcome(welcome)],
        ProtocolMessage::Snapshot(snapshot) => vec![NetEvent::Snapshot(snapshot)],
        ProtocolMessage::Event(event) => vec![NetEvent::Game(event)],
        ProtocolMessage::RoundState(state) => vec![NetEvent::RoundState(state)],
        ProtocolMessage::Pong(pong) => latency
            .on_pong(pong.client_time_ms)
            .map(|rtt| vec![NetEvent::Latency(rtt)])
            .unwrap_or_default(),
        ProtocolMessage::Error(error) => vec![NetEvent::ServerError(error)],
        ProtocolMessage::Unknown(value) => {
            tracing::debug!(?value, "ignoring unknown server message");
            Vec::new()
        }
        // A client->server message arriving from the server is nonsense.
        other => {
            tracing::warn!(
                kind = other.type_name(),
                "unexpected server->client message"
            );
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::message::Pong;

    #[test]
    fn pong_produces_latency_event() {
        let mut latency = LatencyEstimator::default();
        let start = LatencyEstimator::now_ms();
        let events = classify(
            ProtocolMessage::Pong(Pong {
                client_time_ms: start,
                server_time_ms: None,
            }),
            &mut latency,
        );
        assert!(matches!(events.as_slice(), [NetEvent::Latency(_)]));
    }

    #[test]
    fn unknown_messages_are_ignored() {
        let mut latency = LatencyEstimator::default();
        let events = classify(
            ProtocolMessage::from_json_str(r#"{"type":"whatever"}"#).unwrap(),
            &mut latency,
        );
        assert!(events.is_empty());
    }
}
