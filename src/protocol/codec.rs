//! Lenient frame decoding for the inbound direction.
//!
//! [`ProtocolMessage::from_json_str`] is the strict path: one malformed frame
//! kills the whole message. That is wrong for a live game – a single unknown
//! `phase`, a `NaN` or a field the server renamed must never cost the session.
//!
//! [`parse_frame`] therefore decodes the `type` first and decodes only the
//! matching variant, so:
//!
//! * an unknown `type` (e.g. `pusher:connection_established`) is
//!   [`ProtocolMessage::Unknown`],
//! * a frame that has the right `type` but a broken body is a
//!   [`DecodeFailure`] – the caller logs it and keeps the session alive.

use serde::de::DeserializeOwned;

use crate::protocol::message::{ProtocolMessage, RoundState};

/// Why a frame with a known `type` could not be decoded.
#[derive(Debug, thiserror::Error)]
pub enum DecodeFailure {
    #[error("invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("frame is not a JSON object")]
    NotAnObject,
    #[error("frame has no string \"type\"")]
    MissingType,
    #[error("decoding `{type_name}` failed: {source}")]
    Body {
        type_name: &'static str,
        #[source]
        source: serde_json::Error,
    },
}

/// Decodes one socket frame.
pub fn parse_frame(raw: &str) -> Result<ProtocolMessage, DecodeFailure> {
    let value: serde_json::Value = serde_json::from_str(raw)?;
    let Some(object) = value.as_object() else {
        return Err(DecodeFailure::NotAnObject);
    };
    let Some(type_name) = object.get("type").and_then(|t| t.as_str()) else {
        // Frames without a `type` are not from the game protocol – the legacy
        // own envelopes look like `{"event":"pusher:…","data":…}`. Absorb them
        // instead of treating them as protocol errors.
        if object.contains_key("event") {
            return Ok(ProtocolMessage::Unknown(value));
        }
        return Err(DecodeFailure::MissingType);
    };

    match type_name {
        "hello" => {
            decode::<crate::protocol::message::Hello>(value, "hello").map(ProtocolMessage::Hello)
        }
        "input" => decode::<crate::protocol::message::InputMessage>(value, "input")
            .map(ProtocolMessage::Input),
        "ping" => {
            decode::<crate::protocol::message::Ping>(value, "ping").map(ProtocolMessage::Ping)
        }
        "welcome" => decode::<crate::protocol::message::Welcome>(value, "welcome")
            .map(ProtocolMessage::Welcome),
        "snapshot" => decode::<crate::protocol::message::Snapshot>(value, "snapshot")
            .map(ProtocolMessage::Snapshot),
        "event" => decode::<crate::protocol::message::GameEvent>(value, "event")
            .map(ProtocolMessage::Event),
        "round_state" => RoundState::from_wire(value)
            .map(ProtocolMessage::RoundState)
            .map_err(|source| DecodeFailure::Body {
                type_name: "round_state",
                source,
            }),
        "pong" => {
            decode::<crate::protocol::message::Pong>(value, "pong").map(ProtocolMessage::Pong)
        }
        "error" => decode::<crate::protocol::message::ServerError>(value, "error")
            .map(ProtocolMessage::Error),
        _ => Ok(ProtocolMessage::Unknown(value)),
    }
}

fn decode<T: DeserializeOwned>(
    value: serde_json::Value,
    type_name: &'static str,
) -> Result<T, DecodeFailure> {
    serde_json::from_value(value).map_err(|source| DecodeFailure::Body { type_name, source })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_types_decode() {
        let raw = r#"{"type":"pong","client_time_ms":5}"#;
        assert!(matches!(
            parse_frame(raw).unwrap(),
            ProtocolMessage::Pong(_)
        ));
    }

    #[test]
    fn unknown_types_are_absorbed() {
        let raw = r#"{"event":"pusher:connection_established","data":"{}"}"#;
        let parsed = parse_frame(raw).unwrap();
        assert!(parsed.type_name() == "unknown");
    }

    #[test]
    fn broken_body_is_isolated_not_fatal() {
        let raw = r#"{"type":"round_state","phase":"teleporting"}"#;
        match parse_frame(raw) {
            Err(DecodeFailure::Body { type_name, .. }) => assert_eq!(type_name, "round_state"),
            other => panic!("expected a body failure, got {other:?}"),
        }
    }

    #[test]
    fn missing_type_is_reported() {
        assert!(matches!(
            parse_frame(r#"{"x":1}"#),
            Err(DecodeFailure::MissingType)
        ));
        assert!(matches!(
            parse_frame("[1,2]"),
            Err(DecodeFailure::NotAnObject)
        ));
    }
}
