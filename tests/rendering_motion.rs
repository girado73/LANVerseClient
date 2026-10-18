//! Rendering motion: the camera must not sweep, and what is drawn must move
//! smoothly.
//!
//! Reported symptom: the game "flickered" badly right after starting. Three
//! independent causes, all covered here:
//!
//! 1. the camera started at the world origin and *interpolated* to the player
//!    (hundreds of world units per frame ⇒ the whole map sweeps across the
//!    screen),
//! 2. the local player was rendered from the interpolation buffer on the wrong
//!    timeline (client tick vs. server tick),
//! 3. prediction was never active (`Predictor::on_snapshot` was not called), so
//!    the local sprite lagged and jumped on every snapshot.
//!
//! The measurement uses `ClientApp::rendered_positions`, i.e. exactly the
//! screen coordinates the renderer would draw.

use std::time::{Duration, Instant};

use lan_bash_client::app::ClientApp;
use lan_bash_client::config::ClientConfig;
use lan_bash_client::net::mock::MockServerConfig;

/// One loop iteration is one 144 fps frame, so client and mock server advance
/// in step. Returns the worst per-frame movement in internal screen pixels.
fn run_frames(app: &mut ClientApp, frames: usize) -> (f32, String) {
    let target = Duration::from_micros(6_944);
    let mut previous: std::collections::HashMap<String, lan_bash_client::core::Vec2> =
        Default::default();
    let mut worst = (0.0f32, String::new());
    let mut previous_start = Instant::now();

    for _ in 0..frames {
        let start = Instant::now();
        let frame_time = start - previous_start;
        previous_start = start;

        app.frame(frame_time, Default::default());

        for (id, position) in app.rendered_positions() {
            if let Some(previous) = previous.get(id.as_str()) {
                let delta = (*previous - position).length();
                if delta > worst.0 {
                    worst = (delta, id.as_str().to_owned());
                }
            }
            previous.insert(id.as_str().to_owned(), position);
        }

        let spent = start.elapsed();
        if spent < target {
            std::thread::sleep(target - spent);
        }
    }
    worst
}

#[test]
fn camera_snaps_to_the_player_instead_of_sweeping_across_the_map() {
    let runtime = lan_bash_client::app::runtime::build_tokio_runtime().unwrap();
    let _guard = runtime.enter();

    let mut app = ClientApp::new(ClientConfig::default());
    app.connect_mock(MockServerConfig {
        snapshot_interval_ms: 20,
        ..Default::default()
    });

    // Wait for the first snapshot (the camera is not placed before that).
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && app.state().local_player.is_none() {
        app.frame(Duration::from_micros(6_944), Default::default());
        std::thread::sleep(Duration::from_millis(2));
    }

    let local = app
        .state()
        .local_player()
        .expect("no local player after 5 s")
        .position;
    // The camera must already be *at* the player on the first drawn frame: a
    // smooth pan from the origin is what looked like flickering.
    assert!(
        (app.camera().center - local).length() < 1.0,
        "camera did not snap to the player: camera={:?} player={local:?}",
        app.camera().center
    );
}

#[test]
fn drawn_players_move_smoothly_once_the_game_is_running() {
    let runtime = lan_bash_client::app::runtime::build_tokio_runtime().unwrap();
    let _guard = runtime.enter();

    let mut app = ClientApp::new(ClientConfig::default());
    app.connect_mock(MockServerConfig {
        snapshot_interval_ms: 50,
        ..Default::default()
    });

    // Let the session come up and the camera settle (the first frames include
    // the one-time placement), then measure.
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && app.state().players.len() < 2 {
        app.frame(Duration::from_micros(6_944), Default::default());
        std::thread::sleep(Duration::from_millis(2));
    }

    // ~1.4 s of rendered frames.
    let (worst, id) = run_frames(&mut app, 200);
    // Bots in the mock server move slowly; one internal pixel per frame is
    // already generous (the screen is only 480×270). Before the fixes this was
    // 11+ px because of the camera sweep / wrong timeline.
    assert!(
        worst < 4.0,
        "rendered motion jumps by {worst:.1} px per frame on {id} (expected < 4)"
    );
}
