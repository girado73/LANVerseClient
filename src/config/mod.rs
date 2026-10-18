//! Client configuration.
//!
//! Resolution order (highest priority first):
//! 1. command line arguments (`--server ws://…`, `--player-name …`)
//! 2. environment variables (`LAN_BASH_SERVER`, `LAN_BASH_REVERB_KEY`,
//!    `LAN_BASH_PLAYER_NAME`, `LAN_BASH_TOKEN`)
//! 3. built-in defaults
//!
//! Keeping configuration in a dedicated module means every other module takes
//! its limits/rates as arguments instead of reading globals.

pub mod settings;

pub use settings::{
    ClientArgs, ClientConfig, ConnectionConfig, DEFAULT_REVERB_APP_KEY, DEFAULT_REVERB_PORT,
    InputConfig, NetConfig, ReconnectPolicy, RenderConfig, normalize_server_url,
};
