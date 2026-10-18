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
    ///
    /// Sized for the worst case where the server processes inputs far slower
    /// than the client sends them (it acks on snapshots, so a long delay can
    /// pile up a second of inputs). Overflow only costs prediction accuracy.
    pub input_queue_capacity: usize,
    /// Size of the interpolation buffer; ~1 s of snapshots at 20 Hz.
    pub interpolation_buffer_size: usize,
}

impl NetConfig {
    /// ~5 s of inputs at [`crate::simulation::DEFAULT_TICK_RATE`] (64 bytes each).
    ///
    /// The buffer only matters while snapshots are delayed (LAN: a few ms), so
    /// this is already generous; it exists to never drop inputs under normal
    /// play. Use the HUD's overflow counter to spot pathological servers.
    pub const DEFAULT_INPUT_QUEUE_CAPACITY: usize = 300;
}

#[derive(Debug, Clone)]
pub struct ConnectionConfig {
    /// WebSocket endpoint of the Laravel game server.
    ///
    /// **The Reverb app key is required**: `game:serve` routes the game
    /// protocol behind `/app/{key}`, so `ws://127.0.0.1:8080/app` is rejected
    /// with HTTP 403 and `/app/<key>` is the only working endpoint.
    pub url: String,
    pub reconnect: ReconnectPolicy,
    /// Time without any server message before the client assumes the link is dead
    /// and forces a reconnect. The server sends snapshots at 20 Hz and answers
    /// pings every 2 s, so anything above a few seconds is a dead link.
    pub heartbeat_timeout: Duration,
    /// Time allowed for the TCP/WebSocket upgrade itself.
    pub connect_timeout: Duration,
    /// Time allowed between "socket open" and `welcome`.
    ///
    /// A server that accepts the upgrade but never answers `hello` (wrong
    /// routing setup, Reverb without the game protocol) would otherwise look
    /// like a connected client that just does not move.
    pub handshake_timeout: Duration,
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
    /// Frame cap. `0` (default) lets VSync pace the loop – setting both a cap
    /// and VSync makes raylib wait twice per frame, which shows up as judder.
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
            input_queue_capacity: Self::DEFAULT_INPUT_QUEUE_CAPACITY,
            interpolation_buffer_size: 64,
        }
    }
}

/// Reverb app key of a default `php artisan game:serve` installation.
///
/// Override it with `--server`, `LAN_BASH_SERVER` or `LAN_BASH_REVERB_KEY`; the
/// authoritative value lives in the server's `.env` (`REVERB_APP_KEY`).
pub const DEFAULT_REVERB_APP_KEY: &str = "sky7dm0tjxsre1lpqwjg";
/// Port `game:serve` listens on by default.
pub const DEFAULT_REVERB_PORT: u16 = 8080;

impl Default for ConnectionConfig {
    fn default() -> Self {
        Self {
            url: format!("ws://127.0.0.1:{DEFAULT_REVERB_PORT}/app/{DEFAULT_REVERB_APP_KEY}"),
            reconnect: ReconnectPolicy::default(),
            // 20 Hz snapshots + 2 s pings: 8 s means ~160 missed snapshots.
            heartbeat_timeout: Duration::from_secs(8),
            connect_timeout: Duration::from_secs(5),
            handshake_timeout: Duration::from_secs(5),
        }
    }
}

impl Default for ReconnectPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            base_delay: Duration::from_millis(500),
            // Capped at ≈2 s so a LAN server restart is picked up quickly but
            // an offline client does not hammer the socket.
            max_delay: Duration::from_secs(2),
            // 0 = retry forever: opening the client before the server is a
            // normal situation in a LAN, not an error.
            max_attempts: 0,
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
            target_fps: 0,
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
                // Budgets are parsed in `main`; validated here so typos fail early.
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
        let mut app_key = None;

        if let Ok(key) = std::env::var("LAN_BASH_REVERB_KEY") {
            app_key = Some(key);
        }
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
            config.net.connection.url = normalize_server_url(&url, app_key.as_deref());
        } else {
            config.net.connection.url =
                normalize_server_url(&config.net.connection.url, app_key.as_deref());
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

/// Accepts the shorthand URLs people actually type and produces the endpoint
/// the server expects.
///
/// * `127.0.0.1:8080`               → `ws://127.0.0.1:8080/app/<key>`
/// * `ws://127.0.0.1:8080/app`      → `ws://127.0.0.1:8080/app/<key>`
/// * `http://lan.host:8080`         → `ws://lan.host:8080/app/<key>`
/// * `ws://host:8080/app/<key>`     → unchanged
/// * `wss://…`                      → unchanged
///
/// Registering without the key is the single most common misconfiguration:
/// every request then fails with HTTP 403, which the client reports as a
/// transport error before any game frame can arrive.
pub fn normalize_server_url(raw: &str, app_key: Option<&str>) -> String {
    let raw = raw.trim();
    if raw.is_empty() {
        return ConnectionConfig::default().url;
    }

    let key = app_key
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .unwrap_or(DEFAULT_REVERB_APP_KEY);

    let with_scheme = if raw.contains("://") {
        raw.to_owned()
    } else {
        format!("ws://{raw}")
    };
    let normalized = with_scheme
        .replacen("https://", "wss://", 1)
        .replacen("http://", "ws://", 1);

    // `ws://host/…`: the path must end with `/app/<key>`.
    match normalized.rfind("/app") {
        Some(index) => {
            let (base, path) = normalized.split_at(index);
            let rest = path.trim_start_matches("/app").trim_end_matches('/');
            // The key (if any) is the segment after `/app`.
            let existing_key = rest
                .strip_prefix('/')
                .unwrap_or("")
                .split('/')
                .next()
                .filter(|segment| !segment.is_empty());
            if let Some(existing_key) = existing_key {
                format!("{base}/app/{existing_key}")
            } else {
                format!("{base}/app/{key}")
            }
        }
        None => format!("{}/app/{key}", normalized.trim_end_matches('/')),
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
    -s, --server <URL>         Game server endpoint. `host:port`, `ws://host:port/app`
                               or the full `ws://host:port/app/<reverb-key>`
                               (default: {default_url})
    -n, --player-name <NAME>   Display name sent in the handshake
        --token <TOKEN>        Auth token forwarded to Laravel
        --guest                Connect without token under a random guest name
        --headless             Run without a window (network/prediction only)
        --mock-server          Use an in-memory demo server instead of Laravel
        --max-ticks <N>        Headless: stop after N simulated ticks
        --max-frames <N>       GUI: stop after N rendered frames (smoke tests)
    -h, --help                 Show this help

ENVIRONMENT:
    LAN_BASH_SERVER       endpoint; a bare `host:port` gets `/app/<key>` appended
    LAN_BASH_REVERB_KEY   Reverb app key from the server's .env (REVERB_APP_KEY)
    LAN_BASH_PLAYER_NAME, LAN_BASH_TOKEN
    RUST_LOG              tracing filter (e.g. RUST_LOG=lan_bash_client=debug)",
        default_url = ConnectionConfig::default().url
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

    #[test]
    fn default_reconnect_retries_forever_every_two_seconds_at_most() {
        let policy = ReconnectPolicy::default();
        assert_eq!(policy.max_attempts, 0, "0 means unlimited");
        assert_eq!(policy.delay_for(1), Duration::from_millis(500));
        assert_eq!(policy.delay_for(3), Duration::from_secs(2));
        assert_eq!(policy.delay_for(1000), Duration::from_secs(2));
    }

    #[test]
    fn default_url_targets_game_serve() {
        let url = ConnectionConfig::default().url;
        assert!(url.starts_with("ws://127.0.0.1:8080/app/"));
        assert!(url.len() > "ws://127.0.0.1:8080/app/".len());
    }

    #[test]
    fn server_url_shorthand_is_expanded() {
        let key = Some("abc123");
        assert_eq!(
            normalize_server_url("127.0.0.1:8080", key),
            "ws://127.0.0.1:8080/app/abc123"
        );
        assert_eq!(
            normalize_server_url("ws://127.0.0.1:8080/app", key),
            "ws://127.0.0.1:8080/app/abc123"
        );
        assert_eq!(
            normalize_server_url("http://lan.local:8080/", key),
            "ws://lan.local:8080/app/abc123"
        );
        assert_eq!(
            normalize_server_url("ws://host:8080/app/other", key),
            "ws://host:8080/app/other",
            "an explicit key wins over the configured one"
        );
        assert_eq!(
            normalize_server_url("wss://play.example.com/app/", None),
            format!("wss://play.example.com/app/{DEFAULT_REVERB_APP_KEY}")
        );
    }
}
