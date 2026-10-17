use std::time::Duration;

/// Root configuration object handed to [`crate::app::ClientApp::new`].
///
/// Every subsystem receives only the slice it needs:
/// `net`, `input`, `render` – so tests can build them with defaults.
#[derive(Debug, Clone)]
pub struct ClientConfig {
    pub net: NetConfig,
    pub input: InputConfig,
    pub render: RenderConfig,
    pub player_name: String,
    /// Optional bearer token handed to Laravel on the handshake (e.g. Laravel
    /// Sanctum token). The server decides what to do with it.
    pub auth_token: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NetConfig {
    pub connection: ConnectionConfig,
    /// Distance (in ticks) between two local input samples.
    pub input_interval_ticks: u64,
    /// Upper bound of queued inputs so a stalled network cannot grow memory
    /// unbounded. Older inputs are dropped first (they are already worthless).
    pub input_queue_capacity: usize,
    /// Size of the interpolation buffer; ~1 s of snapshots at 20 Hz.
    pub interpolation_buffer_size: usize,
}

#[derive(Debug, Clone)]
pub struct ConnectionConfig {
    /// WebSocket endpoint of the Laravel game server, e.g. `ws://127.0.0.1:8000/app/game`.
    pub url: String,
    pub reconnect: ReconnectPolicy,
    /// Time without any server message before the client assumes the link is dead
    /// and forces a reconnect. Laravel/Echo pings periodically; tune to the server.
    pub heartbeat_timeout: Duration,
}

#[derive(Debug, Clone)]
pub struct ReconnectPolicy {
    pub enabled: bool,
    pub base_delay: Duration,
    pub max_delay: Duration,
    pub max_attempts: u32,
}

impl ReconnectPolicy {
    /// Exponential backoff: `base * 2^(attempt-1)` clamped to `max_delay`.
    pub fn delay_for(&self, attempt: u32) -> Duration {
        let exponent = attempt.saturating_sub(1).min(16);
        let delay = self.base_delay.saturating_mul(1 << exponent);
        delay.min(self.max_delay)
    }
}

#[derive(Debug, Clone)]
pub struct InputConfig {
    /// Movement magnitudes below this are treated as zero (controller deadzone).
    pub deadzone: f32,
}

#[derive(Debug, Clone)]
pub struct RenderConfig {
    /// Native resolution of the pixelart raster. Everything is drawn here and
    /// then scaled to the window with nearest-neighbour filtering, so pixels
    /// always stay crisp regardless of window size.
    pub internal_width: i32,
    pub internal_height: i32,
    pub window_width: i32,
    pub window_height: i32,
    pub title: String,
    pub target_fps: u32,
    pub camera_zoom: f32,
    pub show_debug_overlay: bool,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            net: NetConfig::default(),
            input: InputConfig::default(),
            render: RenderConfig::default(),
            player_name: "Player".to_owned(),
            auth_token: None,
        }
    }
}

impl Default for NetConfig {
    fn default() -> Self {
        Self {
            connection: ConnectionConfig::default(),
            input_interval_ticks: 1,
            input_queue_capacity: 256,
            interpolation_buffer_size: 64,
        }
    }
}

impl Default for ConnectionConfig {
    fn default() -> Self {
        Self {
            url: "ws://127.0.0.1:8000/app/lan-bash/game".to_owned(),
            reconnect: ReconnectPolicy::default(),
            heartbeat_timeout: Duration::from_secs(15),
        }
    }
}

impl Default for ReconnectPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            base_delay: Duration::from_millis(500),
            max_delay: Duration::from_secs(10),
            max_attempts: 12,
        }
    }
}

impl Default for InputConfig {
    fn default() -> Self {
        Self { deadzone: 0.2 }
    }
}

impl Default for RenderConfig {
    fn default() -> Self {
        Self {
            internal_width: 480,
            internal_height: 270,
            window_width: 1440,
            window_height: 810,
            title: "LAN Bash".to_owned(),
            target_fps: 144,
            camera_zoom: 1.0,
            show_debug_overlay: true,
        }
    }
}

/// Parsed command line arguments.
#[derive(Debug, Clone, Default)]
pub struct ClientArgs {
    pub server_url: Option<String>,
    pub player_name: Option<String>,
    pub token: Option<String>,
    /// `true` runs the authoritative-free loop without opening a raylib window
    /// (useful in CI and on headless LAN machines).
    pub headless: bool,
    pub guest: bool,
}

impl ClientArgs {
    pub fn parse<I: IntoIterator<Item = String>>(args: I) -> anyhow::Result<Self> {
        let mut parsed = Self::default();
        let mut iter = args.into_iter().peekable();
        // `args` includes the binary name; it is not a flag.
        let _ = iter.next();

        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--server" | "-s" => parsed.server_url = Some(require_value(&mut iter, &arg)?),
                "--player-name" | "-n" => {
                    parsed.player_name = Some(require_value(&mut iter, &arg)?)
                }
                "--token" => parsed.token = Some(require_value(&mut iter, &arg)?),
                "--headless" => parsed.headless = true,
                "--guest" => parsed.guest = true,
                // Handled by `main`; accepted here for validation.
                "--mock-server" => {}
                "--max-ticks" | "--max-frames" => {
                    let value = require_value(&mut iter, &arg)?;
                    value
                        .parse::<u64>()
                        .map_err(|_| anyhow::anyhow!("`{arg}` expects a number, got `{value}`"))?;
                }
                "--help" | "-h" => {
                    print_help();
                    std::process::exit(0);
                }
                other => anyhow::bail!("unknown argument `{other}` (use --help)"),
            }
        }

        Ok(parsed)
    }

    /// Merges CLI args on top of environment variables and defaults.
    pub fn into_config(self) -> ClientConfig {
        let mut config = ClientConfig::default();

        if let Ok(url) = std::env::var("LAN_BASH_SERVER") {
            config.net.connection.url = url;
        }
        if let Ok(name) = std::env::var("LAN_BASH_PLAYER_NAME") {
            config.player_name = name;
        }
        if let Ok(token) = std::env::var("LAN_BASH_TOKEN") {
            config.auth_token = Some(token);
        }

        if let Some(url) = self.server_url {
            config.net.connection.url = url;
        }
        if let Some(name) = self.player_name {
            config.player_name = name;
        }
        if let Some(token) = self.token {
            config.auth_token = Some(token);
        }
        if self.guest {
            config.auth_token = None;
            config.player_name = format!("Guest-{}", random_suffix());
        }

        config
    }
}

fn require_value<I: Iterator<Item = String>>(iter: &mut I, flag: &str) -> anyhow::Result<String> {
    iter.next()
        .ok_or_else(|| anyhow::anyhow!("missing value for `{flag}`"))
}

fn random_suffix() -> u16 {
    use std::time::{SystemTime, UNIX_EPOCH};
    (SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0)
        % 10_000) as u16
}

fn print_help() {
    println!(
        "\
LAN Bash client

USAGE:
    lan_bash_client [OPTIONS]

OPTIONS:
    -s, --server <URL>         WebSocket URL of the Laravel game server
    -n, --player-name <NAME>   Display name sent in the handshake
        --token <TOKEN>        Auth token forwarded to Laravel
        --guest                Connect without token under a random guest name
        --headless             Run without a window (network/prediction only)
        --mock-server          Use an in-memory demo server instead of Laravel
        --max-ticks <N>        Headless: stop after N simulated ticks
        --max-frames <N>       GUI: stop after N rendered frames (screenshots/tests)
    -h, --help                 Show this help

ENVIRONMENT:
    LAN_BASH_SERVER, LAN_BASH_PLAYER_NAME, LAN_BASH_TOKEN
    RUST_LOG (tracing filter, e.g. RUST_LOG=lan_bash_client=debug)"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cli_arguments() {
        let args = ClientArgs::parse([
            "client".to_string(),
            "--server".to_string(),
            "ws://example:9000/ws".to_string(),
            "--player-name".to_string(),
            "Alice".to_string(),
            "--headless".to_string(),
        ])
        .unwrap();

        assert_eq!(args.server_url.as_deref(), Some("ws://example:9000/ws"));
        assert!(args.headless);
    }

    #[test]
    fn rejects_unknown_arguments() {
        assert!(ClientArgs::parse(["client".to_string(), "--nope".to_string()]).is_err());
    }

    #[test]
    fn backoff_grows_exponentially_and_is_capped() {
        let policy = ReconnectPolicy {
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_millis(1000),
            ..Default::default()
        };
        assert_eq!(policy.delay_for(1), Duration::from_millis(100));
        assert_eq!(policy.delay_for(2), Duration::from_millis(200));
        assert_eq!(policy.delay_for(4), Duration::from_millis(800));
        assert_eq!(policy.delay_for(10), Duration::from_millis(1000));
    }
}
