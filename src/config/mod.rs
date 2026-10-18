//! Client configuration.
//!
//! Resolution order (highest priority first):
//! 1. command line arguments (`--server ws://…`, `--player-name …`)
//! 2. environment variables (`LAN_BASH_SERVER`,
//!    `LAN_BASH_PLAYER_NAME`, `LAN_BASH_TOKEN`)
//! 3. built-in defaults
//!
//! Keeping configuration in a dedicated module means every other module takes
//! its limits/rates as arguments instead of reading globals.

pub mod settings;

pub use settings::{
    ClientArgs, ClientConfig, ConnectionConfig, DEFAULT_GAME_PORT, InputConfig, NetConfig,
    ReconnectPolicy, RenderConfig, normalize_server_url,
};
