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
///
/// The variants only ever fail to deserialize for *semantic* reasons (e.g. an
/// unknown `phase`). Unknown `type`s end up in [`ProtocolMessage::Unknown`] and
/// are ignored by [`crate::net::events::classify`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProtocolMessage {
    // ---- client -> server ----
    /// First message after the socket opens. The server validates it (token,
    /// match availability) and answers with [`ProtocolMessage::Welcome`].
    Hello(Hello),
    /// One input sample per simulated tick.
    Input(InputMessage),
    /// Latency probe; answered by [`ProtocolMessage::Pong`].
    Ping(Ping),
    /// Asks the server for the current match's Lua package (offline copy).
    Pull(Pull),

    // ---- server -> client ----
    /// Handshake accepted.
    Welcome(Welcome),
    /// Authoritative world state.
    Snapshot(Snapshot),
    /// Mode package download, answering [`ProtocolMessage::Pull`].
    Mode(ModePayload),
    /// Discrete gameplay event.
    Event(GameEvent),
    /// Match lifecycle transition.
    RoundState(RoundState),
    Pong(Pong),
    /// Known error.
    Error(ServerError),

    /// Unknown/unhandled message type – ignored by the client.
    ///
    /// This must stay the **last** variant: serde tries untagged variants after
    /// every tagged one, so it only ever matches frames whose `type` is unknown.
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
            Self::Pull(_) => "pull",
            Self::Mode(_) => "mode",
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
    /// Account token (optional for guest play; the server decides).
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

    /// Joins a specific match. Uploaded Lua modes are matches named after the
    /// mode id (`coin_rush`); `None` keeps the server's auto matchmaking.
    pub fn with_match(mut self, match_id: Option<String>) -> Self {
        self.match_id = match_id.map(crate::core::MatchId::from);
        self
    }
}

/// Asks for the current match's mode package.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Pull {
    /// Always "mode": more pull targets can be added without breaking clients.
    #[serde(default = "default_pull_target")]
    pub what: String,
}

fn default_pull_target() -> String {
    "mode".to_owned()
}

impl Pull {
    pub fn mode() -> Self {
        Self {
            what: default_pull_target(),
        }
    }
}

/// Mode package sent by the server (`type: "mode"`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ModePayload {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<ModePackage>,
}

/// The builder's `mode.json` plus its Lua source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModePackage {
    pub id: String,
    #[serde(default)]
    pub name: String,
    /// The builder manifest. Stored verbatim; the client does not interpret it
    /// (the server is authoritative for world size and rules).
    #[serde(default)]
    pub manifest: serde_json::Value,
    #[serde(default)]
    pub script: String,
    #[serde(default)]
    pub checksum: String,
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
    /// Optional authoritative world size in world units (the server's `world`
    /// config is 3200×1800). The server does not send it yet; when absent the
    /// client falls back to its default playfield.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub world: Option<WorldState>,
    /// Optional movement speed in world units per second, used only for local
    /// *visual* prediction. The server does not send it yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub move_speed: Option<f32>,
    /// Lua mode package currently played by the match, if any.
    ///
    /// Only the checksum and id: the full package is fetched through the REST
    /// API on demand, so normal play never depends on the script download.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<ModeSummary>,
}

/// Which Lua mode a match runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModeSummary {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub checksum: String,
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
    /// Authoritative world size, if the server chose to include it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub world: Option<WorldState>,
    /// Markers placed by a Lua mode script. Absent for the built-in game.
    ///
    /// Players may edit their local copy (they are plain Lua tables on the
    /// server), so every field is validated before it is trusted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub markers: Vec<crate::models::world::MarkerState>,
}

/// Immutable world metadata mirrored from the server.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct WorldState {
    pub width: f32,
    pub height: f32,
}

/// `round` object inside a snapshot.
///
/// The server sends `{"round_id": "lan-20261017", "time_left_ms": 179000}`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RoundSummary {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub round_id: Option<RoundId>,
    #[serde(default)]
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
    /// Lua script console output (`lb.log`, `print`, `lb.warn`).
    ModeLog {
        #[serde(default)]
        mode_id: String,
        #[serde(default)]
        level: String,
        #[serde(default)]
        message: String,
    },
    /// A mode script failed; the match fell back to Deathmatch rules.
    ModeError {
        #[serde(default)]
        mode_id: String,
        #[serde(default)]
        message: String,
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

/// `{"type": "round_state"}` – match lifecycle.
///
/// The server always includes `phase` and `countdown_ms` (`0` outside the
/// countdown) and omits `round`/`round_number`/`winner` while they are unset.
/// `countdown_ms: 0` therefore means "not counting down", *not* "time is up",
/// which is why this is an [`Option`] with an explicit zero → [`None`] mapping
/// in [`RoundState::from_wire`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoundState {
    pub phase: Phase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub round: Option<RoundId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub round_number: Option<u32>,
    /// Remaining countdown, only meaningful while `phase == countdown`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub countdown_ms: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub winner: Option<String>,
}

impl RoundState {
    /// Normalizes the wire representation: the server sends `round_number: 0` and
    /// `countdown_ms: 0` instead of omitting them. Neither is a real value.
    pub fn from_wire(raw: serde_json::Value) -> serde_json::Result<Self> {
        let mut state: Self = serde_json::from_value(raw)?;
        state.round_number = state.round_number.filter(|n| *n > 0);
        state.countdown_ms = state.countdown_ms.filter(|ms| *ms > 0);
        Ok(state)
    }
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
    /// Text frames with JSON (default; human readable).
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

    #[test]
    fn welcome_parses_without_new_optional_fields() {
        // Exactly what WelcomeMessage::toArray() produces.
        let raw = r#"{"type":"welcome","protocol_version":1,"player_id":"p-7",
                      "session_id":"s-1","match_id":"lan-20261017",
                      "tick_rate":60,"server_tick":0,"message":"Welcome"}"#;
        let ProtocolMessage::Welcome(welcome) = ProtocolMessage::from_json_str(raw).unwrap() else {
            panic!("expected welcome");
        };
        assert_eq!(welcome.player_id, PlayerId::from("p-7"));
        assert!(welcome.world.is_none());
        assert!(welcome.move_speed.is_none());
    }

    #[test]
    fn round_state_parses() {
        let raw = r#"{"type":"round_state","phase":"countdown","round":"lan-20261017",
                      "round_number":1,"countdown_ms":3000}"#;
        let state = RoundState::from_wire(serde_json::from_str(raw).unwrap()).unwrap();
        assert_eq!(state.phase, Phase::Countdown);
        assert_eq!(state.round_number, Some(1));
        assert_eq!(state.countdown_ms, Some(3000));
    }

    #[test]
    fn zero_countdown_means_not_counting() {
        let raw = r#"{"type":"round_state","phase":"running","round":"r","round_number":1,"countdown_ms":0}"#;
        let state = RoundState::from_wire(serde_json::from_str(raw).unwrap()).unwrap();
        assert_eq!(state.countdown_ms, None);
        // The server omits round/round_number before a round exists.
        let raw = r#"{"type":"round_state","phase":"lobby","countdown_ms":0}"#;
        let state = RoundState::from_wire(serde_json::from_str(raw).unwrap()).unwrap();
        assert_eq!(state.phase, Phase::Lobby);
        assert_eq!(state.round_number, None);
    }

    #[test]
    fn lua_mode_snapshot_parses_with_markers() {
        let raw = r#"{"type":"snapshot","tick":300,"acked_input_sequence":12,
                      "round":{"round_id":"r-1","time_left_ms":90000},
                      "players":[{"id":"p-1","name":"Alice","position":{"x":480,"y":270},
                      "velocity":{"x":0,"y":0},"facing":{"x":1,"y":0},"hp":100,"max_hp":100,
                      "is_alive":true,"score":3,"team":null}],
                      "markers":[{"id":7,"x":100.5,"y":200,"label":"+1","kind":"coin"}]}"#;
        let ProtocolMessage::Snapshot(snap) = ProtocolMessage::from_json_str(raw).unwrap() else {
            panic!("expected snapshot");
        };
        assert_eq!(snap.markers.len(), 1);
        assert_eq!(snap.markers[0].id, 7);
        assert_eq!(snap.markers[0].kind, "coin");
        assert!(snap.validate().is_ok());
    }

    #[test]
    fn a_deathmatch_snapshot_has_no_markers() {
        let raw = r#"{"type":"snapshot","tick":1,"players":[]}"#;
        let ProtocolMessage::Snapshot(snap) = ProtocolMessage::from_json_str(raw).unwrap() else {
            panic!("expected snapshot");
        };
        assert!(snap.markers.is_empty());
    }

    #[test]
    fn lua_mode_welcome_and_pull_frames_parse() {
        let raw = r#"{"type":"welcome","protocol_version":1,"player_id":"p-1","tick_rate":60,
                      "server_tick":0,"match_id":"coin_rush",
                      "mode":{"id":"coin_rush","name":"Coin Rush","checksum":"abc"}}"#;
        let ProtocolMessage::Welcome(welcome) = ProtocolMessage::from_json_str(raw).unwrap() else {
            panic!("expected welcome");
        };
        let mode = welcome.mode.expect("mode summary");
        assert_eq!(mode.id, "coin_rush");

        let pull = ProtocolMessage::Pull(Pull::mode())
            .to_json_string()
            .unwrap();
        assert_eq!(pull, r#"{"type":"pull","what":"mode"}"#);

        let raw = r#"{"type":"mode","mode":{"id":"coin_rush","name":"Coin Rush",
                      "manifest":{"id":"coin_rush"},"script":"function on_start() end",
                      "checksum":"abc"}}"#;
        let ProtocolMessage::Mode(payload) = ProtocolMessage::from_json_str(raw).unwrap() else {
            panic!("expected mode payload");
        };
        assert_eq!(payload.mode.unwrap().script, "function on_start() end");
    }

    #[test]
    fn snapshot_parses() {
        let raw = r#"{"type":"snapshot","tick":180,"acked_input_sequence":177,
                      "round":{"round_id":"lan-20261017","time_left_ms":180000},
                      "players":[{"id":"p-1","name":"Alice","position":{"x":768.62,"y":630},
                      "velocity":{"x":0,"y":0},"facing":{"x":1,"y":0},"hp":100,"max_hp":100,
                      "is_alive":true,"score":0,"team":null}]}"#;
        let ProtocolMessage::Snapshot(snap) = ProtocolMessage::from_json_str(raw).unwrap() else {
            panic!("expected snapshot");
        };
        assert_eq!(snap.acked_input_sequence, Some(177));
        let round = snap.round.clone().expect("round");
        assert_eq!(round.time_left_ms, 180_000);
        assert_eq!(round.round_id, Some(RoundId::from("lan-20261017")));
        assert_eq!(snap.players[0].team, None);
        snap.validate().unwrap();
    }

    #[test]
    fn events_parse() {
        let hit = r#"{"type":"event","event":"player_hit","attacker":"p-1","victim":"p-2",
                      "damage":12,"remaining_hp":88,"position":{"x":140,"y":92}}"#;
        let ProtocolMessage::Event(GameEvent::PlayerHit { remaining_hp, .. }) =
            ProtocolMessage::from_json_str(hit).unwrap()
        else {
            panic!("expected player_hit");
        };
        assert_eq!(remaining_hp, Some(88));

        let score = r#"{"type":"event","event":"score_changed","player_id":"p-1","score":3}"#;
        let ProtocolMessage::Event(GameEvent::ScoreChanged { team, .. }) =
            ProtocolMessage::from_json_str(score).unwrap()
        else {
            panic!("expected score_changed");
        };
        assert_eq!(team, None);
    }

    #[test]
    fn error_parses() {
        let raw = r#"{"type":"error","code":"auth_failed","message":"Invalid token","fatal":true}"#;
        let ProtocolMessage::Error(error) = ProtocolMessage::from_json_str(raw).unwrap() else {
            panic!("expected error");
        };
        assert_eq!(error.code, "auth_failed");
        assert!(error.fatal);
    }

    #[test]
    fn pusher_envelope_is_unknown_not_an_error() {
        // The server sends `pusher:connection_established` before the game
        // protocol starts; it must never kill the session.
        let raw = r#"{"event":"pusher:connection_established","data":"{\"socket_id\":\"1.2\"}"}"#;
        let msg = ProtocolMessage::from_json_str(raw).unwrap();
        assert!(matches!(msg, ProtocolMessage::Unknown(_)));
    }
}
