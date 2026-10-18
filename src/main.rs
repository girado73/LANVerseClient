//! Entry point: parses configuration and starts the appropriate loop.

use lan_bash_client::app::runtime::{build_tokio_runtime, describe, init_tracing};
use lan_bash_client::app::{ClientApp, ExitReason};
use lan_bash_client::config::ClientArgs;

fn main() -> anyhow::Result<()> {
    init_tracing();

    let raw_args: Vec<String> = std::env::args().collect();
    let args = ClientArgs::parse(raw_args.clone())?;
    let mock_server = raw_args.iter().any(|arg| arg == "--mock-server");
    let headless = args.headless;
    let max_ticks = parse_budget(&raw_args, "--max-ticks");
    let max_frames = parse_budget(&raw_args, "--max-frames");

    let config = args.into_config();
    tracing::info!("starting LAN Bash client: {}", describe(&config));

    if mock_server {
        tracing::info!("using in-memory mock server (no network)");
    }

    let mut app = ClientApp::new(config);
    if mock_server {
        tracing::info!("using in-memory mock server");
        app.connect_mock(lan_bash_client::net::mock::MockServerConfig {
            player_name: app.player_name().to_owned(),
            ..Default::default()
        });
    } else {
        app.connect();
    }

    if headless {
        tracing::info!("headless mode – no window will be opened");
        // The session driver runs on its own thread/runtime; this runtime only
        // keeps the process alive for the duration of the simulated run.
        let _runtime = build_tokio_runtime()?;
        // Give the driver a moment to deliver `welcome` before ticking.
        std::thread::sleep(std::time::Duration::from_millis(50));

        let reason = app.run_headless(max_ticks)?;
        log_summary(&app, reason);
        return Ok(());
    }

    // The GUI loop must run on the main thread – raylib requires it.
    let reason = app.run_gui(max_frames)?;
    log_summary(&app, reason);
    Ok(())
}

fn log_summary(app: &ClientApp, reason: ExitReason) {
    let state = app.state();
    tracing::info!(
        ?reason,
        tick = app.current_tick().0,
        players = state.players.len(),
        phase = ?state.phase,
        "client stopped"
    );
}

/// Parses `--flag <number>` from the raw argv, defaulting to `None`.
fn parse_budget(args: &[String], flag: &str) -> Option<u64> {
    args.iter()
        .position(|arg| arg == flag)
        .and_then(|index| args.get(index + 1))
        .and_then(|value| value.parse().ok())
}
