//! Strongly-typed identifiers.
//!
//! Protocol and models use plain strings/numbers on the wire (`serde`
//! `transparent`), but inside the client identifiers are newtypes so that a
//! `PlayerId` can never be accidentally passed where a `RoundId` is expected.

use std::fmt;

use serde::{Deserialize, Serialize};

macro_rules! string_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_owned())
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }
    };
}

string_id!(
    /// Identifies a connected player (our own player uses the one the server assigns).
    PlayerId
);
string_id!(
    /// Identifies one played match/round on the server.
    MatchId
);
string_id!(
    /// Identifies a single round inside a match.
    RoundId
);
string_id!(
    /// Identifies a client connection/session (handshake token).
    SessionId
);
