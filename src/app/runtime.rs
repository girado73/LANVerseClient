//! Runtime setup: tracing and the Tokio runtime the networking thread uses.

use crate::config::ClientConfig;

/// Initialises `tracing` once, honouring `RUST_LOG` (default: `info`).
///
/// Safe to call multiple times (e.g. in tests) – later calls are no-ops.
pub fn init_tracing() {
    use std::sync::Once;
    static INIT: Once = Once::new();

    INIT.call_once(|| {
        let filter = tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,lan_bash_client=debug"));
        let _ = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_target(true)
            .with_thread_names(true)
            .try_init();
    });
}

/// Creates the multi-threaded runtime used by the headless game loop.
///
/// The GUI path does not use this: `net::session::spawn` starts its own
/// current-thread runtime on a dedicated thread, because raylib must stay on
/// the main thread.
pub fn build_tokio_runtime() -> anyhow::Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .thread_name("lan-bash-runtime")
        .build()?)
}

/// Simple environment summary for the log at startup.
pub fn describe(config: &ClientConfig) -> String {
    format!(
        "server={} player={} internal={}x{} window={}x{}",
        config.net.connection.url,
        config.player_name,
        config.render.internal_width,
        config.render.internal_height,
        config.render.window_width,
        config.render.window_height,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describe_contains_endpoint_and_resolution() {
        let text = describe(&ClientConfig::default());
        assert!(text.contains("ws://"));
        assert!(text.contains("480x270"));
    }

    #[test]
    fn runtime_can_be_built() {
        let runtime = build_tokio_runtime().unwrap();
        runtime.block_on(async { tokio::time::sleep(std::time::Duration::from_millis(1)).await });
    }
}
