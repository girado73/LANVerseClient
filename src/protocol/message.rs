use serde::{Deserialize, Serialize};

use crate::core::{MatchId, PlayerId, RoundId, Tick, Vec2};
use crate::input::InputActions;
use crate::models::world::PlayerState;
use crate::protocol::PROTOCOL_VERSION;

// ---------------------------------------------------------------------------
// Envelope
// ---------------------------------------------------------------------------

/// Any message that can travel over the socket, in either direction.
///
/// Internally tagged: `{"type": "snapshot", "tick": 18420, …}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProtocolMessage {
    // ---- client -> server ----
    /// First message after the socket opens. Laravel validates it (token,
    /// match availability) and answers with [`ProtocolMessage::Welcome`].
    Hello(Hello),
    /// One input sample per simulated tick.
    Input(InputMessage),
    /// Latency probe; answered by [`ProtocolMessage::Pong`].
    Ping(Ping),

    // ---- server -> client ----
    /// Handshake accepted.
    Welcome(Welcome),
    /// Authoritative world state.
    Snapshot(Snapshot),
    /// Discrete gameplay event.
    Event(GameEvent),
    /// Match lifecycle transition.
    RoundState(RoundState),
    Pong(Pong),
    /// Known error.
    Error(ServerError),

    /// Unknown/unhandled message type – ignored by the client.
    #[serde(untagged)]
    Unknown(serde_json::Value),
}

impl ProtocolMessage {
    /// Pretty-prints for logging without failing on partially malformed values.
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Hello(_) => "hello",
            Self::Input(_) => "input",
            Self::Ping(_) => "ping",
            Self::Welcome(_) => "welcome",
            Self::Snapshot(_) => "snapshot",
            Self::Event(_) => "event",
            Self::RoundState(_) => "round_state",
            Self::Pong(_) => "pong",
            Self::Error(_) => "error",
            Self::Unknown(_) => "unknown",
        }
    }

    pub fn from_json_str(raw: &str) -> Result<Self, MessageParseError> {
        serde_json::from_str(raw).map_err(MessageParseError::InvalidJson)
    }

    pub fn to_json_string(&self) -> Result<String, MessageParseError> {
        serde_json::to_string(self).map_err(MessageParseError::Serialize)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum MessageParseError {
    #[error("invalid JSON message: {0}")]
    InvalidJson(#[from] serde_json::Error),
    #[error("failed to serialize message: {0}")]
    Serialize(serde_json::Error),
}

// ---------------------------------------------------------------------------
// Client -> server
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hello {
    pub protocol_version: u32,
    /// Display name shown to other players.
    pub player_name: String,
    /// Laravel auth token (optional for guest play; the server decides).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// Which match to join (lobby auto-assigns when omitted).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub match_id: Option<MatchId>,
    /// Client build string for diagnostics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_version: Option<String>,
}

impl Hello {
    pub fn new(player_name: impl Into<String>, token: Option<String>) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            player_name: player_name.into(),
            token,
            match_id: None,
            client_version: Some(env!("CARGO_PKG_VERSION").to_owned()),
        }
    }
}

/// The AGENTS.md input example:
///
/// ```json
/// { "type": "input", "client_tick": 18420, "sequence": 815,
///   "movement": { "x": 1, "y": 0 }, "actions": [] }
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InputMessage {
    /// The tick the client believes the server is at (client tick + RTT/2).
    pub client_tick: u64,
    /// Monotonic per-connection counter used by the server for reconciliation.
    pub sequence: u64,
    /// Normalized movement intent (length ≤ 1). The server moves the player.
    pub movement: Vec2,
    /// One-shot intents (jump is on key press; dash, use, … are mapped to it).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<InputActions>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Ping {
    /// Client-side timestamp (ms, monotonic-ish) echoed back in the pong.
    pub client_time_ms: u64,
}

// ---------------------------------------------------------------------------
// Server -> client
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Welcome {
    pub protocol_version: u32,
    pub player_id: PlayerId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub match_id: Option<MatchId>,
    /// Server simulation rate. The client aligns its fixed clock to this.
    pub tick_rate: u32,
    /// Server's current simulation tick at the moment of the handshake.
    pub server_tick: u64,
    #[serde(default)]
    pub message: Option<String>,
}

/// One authoritative world state at a given tick.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    /// Server tick this state belongs to. Clients interpolate between snapshots.
    pub tick: u64,
    /// Tick of the last input the server has processed from *us* (reconciliation key).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acked_input_sequence: Option<u64>,
    /// Authoritative players. Full state (the client never guesses).
    pub players: Vec<PlayerState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub round: Option<RoundSummary>,
    /// Advance simulation by `snapshot.tick - previous.tick`.
    /// Unused in the prototype but reserved for rollback-based prediction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_time_ms: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RoundSummary {
    pub round_id: Option<RoundId>,
    pub time_left_ms: u32,
}

/// Discrete gameplay events, replayed by `net::client` and consumed by
/// audio/fx/UI. The client must never invent these.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum GameEvent {
    PlayerJoined {
        player_id: PlayerId,
        #[serde(default)]
        name: String,
    },
    PlayerLeft {
        player_id: PlayerId,
    },
    /// The server confirmed a hit. `damage`/`hp` are authoritative.
    PlayerHit {
        attacker: PlayerId,
        victim: PlayerId,
        damage: u32,
        #[serde(default)]
        remaining_hp: Option<u32>,
        #[serde(default)]
        position: Option<Vec2>,
    },
    ItemPicked {
        player_id: PlayerId,
        item_id: String,
    },
    ScoreChanged {
        player_id: PlayerId,
        score: i32,
        #[serde(default)]
        team: Option<String>,
    },
    /// Unknown future event – absorbed for forward compatibility.
    #[serde(other)]
    Unknown,
}

/// Match lifecycle. Only the server may transition the client into these phases.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Connecting,
    Lobby,
    Countdown,
    Running,
    Finished,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoundState {
    pub phase: Phase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub round: Option<RoundId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub round_number: Option<u32>,
    /// e.g. countdown remaining or final result text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub countdown_ms: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub winner: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pong {
    pub client_time_ms: u64,
    #[serde(default)]
    pub server_time_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerError {
    pub code: String,
    pub message: String,
    /// The server will close the connection after a fatal error.
    #[serde(default)]
    pub fatal: bool,
}

// ---------------------------------------------------------------------------
// Encoding
// ---------------------------------------------------------------------------

/// Frame encoding used on the socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    /// Text frames with JSON (default; human readable, Laravel friendly).
    JsonText,
    /// Binary frames with CBOR (future optimisation, `protocol::codec`).
    CborBinary,
}

impl From<Tick> for u64 {
    fn from(tick: Tick) -> Self {
        tick.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Validate;
    use serde_json::json;

    #[test]
    fn input_example_from_agents_md_round_trips() {
        let raw = r#"{
            "type": "input",
            "client_tick": 18420,
            "sequence": 815,
            "movement": { "x": 1, "y": 0 },
            "actions": []
        }"#;

        let msg = ProtocolMessage::from_json_str(raw).unwrap();
        let ProtocolMessage::Input(input) = msg else {
            panic!("expected input, got {msg:?}");
        };
        assert_eq!(input.client_tick, 18420);
        assert_eq!(input.sequence, 815);
        assert_eq!(input.movement, Vec2::new(1.0, 0.0));
        assert!(input.actions.is_empty());
        input.validate().unwrap();

        // No empty `actions` array is written back out (kept compact).
        let json = serde_json::to_value(ProtocolMessage::Input(input)).unwrap();
        assert_eq!(json["type"], "input");
        assert!(json.get("actions").is_none());
    }

    #[test]
    fn input_with_actions_serializes_named_actions() {
        let msg = InputMessage {
            client_tick: 1,
            sequence: 1,
            movement: Vec2::new(0.0, -1.0),
            actions: vec![InputActions::Jump],
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["actions"], json!(["jump"]));
    }

    #[test]
    fn unknown_message_types_are_absorbed() {
        let msg = ProtocolMessage::from_json_str(r#"{"type":"future_thing","x":1}"#).unwrap();
        assert!(matches!(msg, ProtocolMessage::Unknown(_)));
    }

    #[test]
    fn unknown_events_do_not_break_deserialization() {
        let ev: GameEvent = serde_json::from_value(json!({"event": "exploded"})).unwrap();
        assert_eq!(ev, GameEvent::Unknown);
    }

    #[test]
    fn snapshot_deserializes_with_defaults() {
        let raw = r#"{
            "type": "snapshot", "tick": 12,
            "players": [{"id":"p1","name":"A","position":{"x":4,"y":8},"velocity":{"x":0,"y":0},
                         "facing":{"x":1,"y":0},"hp":100,"max_hp":100}]
        }"#;
        let ProtocolMessage::Snapshot(snap) = ProtocolMessage::from_json_str(raw).unwrap() else {
            panic!("expected snapshot");
        };
        assert_eq!(snap.tick, 12);
        assert_eq!(snap.players.len(), 1);
        assert_eq!(snap.players[0].position, Vec2::new(4.0, 8.0));
        assert!(snap.acked_input_sequence.is_none());
        snap.validate().unwrap();
    }

    #[test]
    fn round_state_phase_is_snake_case() {
        let json = serde_json::to_value(&RoundState {
            phase: Phase::Countdown,
            round: None,
            round_number: Some(1),
            countdown_ms: Some(3000),
            winner: None,
        })
        .unwrap();
        assert_eq!(json["phase"], "countdown");
    }

    #[test]
    fn malformed_json_is_an_error() {
        assert!(ProtocolMessage::from_json_str("{not json").is_err());
    }
}
