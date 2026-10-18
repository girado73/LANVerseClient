//! The game loop itself.
//!
//! Two entry points share all subsystem code:
//!
//! * [`ClientApp::run_gui`] – opens a raylib window, renders the pixelart world.
//! * [`ClientApp::run_headless`] – the same simulation without rendering; used
//!   for CI smoke tests and debugging on machines without a display.
//!
//! Everything that does not depend on raylib lives in [`ClientApp::tick`] and
//! [`ClientApp::frame`], so both modes exercise identical logic.

use std::time::{Duration, Instant};

use raylib::prelude::*;

use crate::audio::Mixer;
use crate::audio::backend::{AudioBackend, NullAudioBackend};
use crate::camera::Camera;
use crate::config::ClientConfig;
use crate::core::{FixedClock, Tick};
use crate::effects::FxEvent;
use crate::input::source::{InputSample, InputSource, ScriptedInputSource};
use crate::input::{InputBuffer, InputPipeline};
use crate::models::GameState;
use crate::net::client::GameClient;
use crate::platform::desktop::{DesktopPlatform, KeyboardInputSource};
use crate::protocol::message::Hello;
use crate::render::hud::HudRenderer;
use crate::render::scene::{SceneRenderer, compute_render_tick};
use crate::simulation::prediction::{MovementModel, Predictor};
use crate::simulation::tick_rate::TickRate;

/// Shared by the GUI loop and diagnostics so both use the same formula.
fn render_tick_of(
    state: &GameState,
    interpolator: &crate::interpolation::Interpolator,
    server_tick: u64,
) -> f32 {
    compute_render_tick(
        server_tick,
        state.latency_ms,
        TickRate::new(state.tick_rate),
        interpolator,
    )
}

/// How long a headless run waits for the handshake before it starts ticking.
const HEADLESS_CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunMode {
    /// Render with a window.
    Gui,
    /// No window, no renderer – simulation and network only.
    Headless,
}

/// Why the loop stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitReason {
    WindowClosed,
    TickBudgetReached,
    Interrupted,
}

pub struct ClientApp {
    config: ClientConfig,
    state: GameState,
    net: GameClient,
    input_pipeline: InputPipeline,
    input_buffer: InputBuffer,
    predictor: Predictor,
    clock: FixedClock,
    camera: Camera,
    mixer: Mixer,
    /// Local tick at which the next input sample is due.
    next_input_tick: u64,
    /// Human-readable notifications for the HUD/debug log.
    notifications: Vec<String>,
    /// Whether `run_*` should connect to the configured server on entry.
    ///
    /// Tests and `--mock-server` call `connect_*` beforehand and turn this off,
    /// so the loop never replaces an already established (or mocked) session.
    connect_automatically: bool,
    /// Snapshot of the ack the server has confirmed (diagnostics).
    last_acked_input: Option<u64>,
    /// `is_alive` of the local player in the previous frame, to detect a
    /// respawn (which is a teleport, not movement).
    local_was_alive: bool,
    /// Whether the camera has a meaningful position for this session.
    ///
    /// The camera starts at the world origin; without a snap it sweeps the
    /// whole map on the first frame the player appears (deltas of hundreds of
    /// units per frame – the "flicker" when starting a game).
    camera_placed: bool,
    /// Offset from the local clock to the server tick (`server - local`).
    ///
    /// Both advance at the same rate, but they start at unrelated values, so
    /// the interpolator needs the server's timeline, not ours. `None` until the
    /// first snapshot arrived in this session.
    tick_offset: Option<i64>,
    /// Mode requested with `--mode` (echoed by the server in `welcome.match_id`).
    configured_mode: Option<String>,
    /// Set once the mode package of the current match was requested.
    ///
    /// Downloading it is optional: the client stores it next to its own modes
    /// but never executes it.
    mode_pulled: bool,
    /// Runtime that polls the network connection.
    ///
    /// The game thread usually has no Tokio reactor, and `run_headless` blocks
    /// it for the whole run, so the app keeps its *own* runtime here (a game
    /// loop may not borrow the caller's – that would never be polled). `None`
    /// means an ambient runtime was provided by the caller (tests).
    runtime: Option<tokio::runtime::Runtime>,
}

impl ClientApp {
    pub fn new(config: ClientConfig) -> Self {
        let prediction_speed = config.net.world_move_speed;
        let config_mode = config.mode.clone();
        let tick_rate = TickRate::default();
        let mut clock = FixedClock::new(tick_rate.hz(), Tick::ZERO);
        // Align `clock.current_tick` with input sampling.
        clock.set_current_tick(Tick::ZERO);

        let net = GameClient::new(config.net.clone());
        let input_buffer = InputBuffer::new(config.net.input_queue_capacity);
        let mut camera = Camera::default();
        camera.set_viewport(
            config.render.internal_width as f32,
            config.render.internal_height as f32,
            config.render.camera_zoom,
        );

        Self {
            input_pipeline: InputPipeline::new(config.input.deadzone),
            next_input_tick: 1,
            config,
            state: GameState::new(),
            net,
            input_buffer,
            predictor: Predictor::new(MovementModel::with_speed(prediction_speed)),
            clock,
            camera,
            mixer: Mixer::default(),
            notifications: Vec::new(),
            connect_automatically: true,
            last_acked_input: None,
            local_was_alive: false,
            camera_placed: false,
            tick_offset: None,
            configured_mode: config_mode,
            mode_pulled: false,
            runtime: None,
        }
    }

    pub fn state(&self) -> &GameState {
        &self.state
    }

    /// Configured player name (for mock server setup and UI).
    pub fn player_name(&self) -> &str {
        &self.config.player_name
    }

    /// Connects to the configured server.
    ///
    /// After an explicit call, `run_gui`/`run_headless` will not connect again.
    pub fn connect(&mut self) {
        self.connect_automatically = false;
        self.connect_with(|net, hello| net.connect(hello));
    }

    /// Connects to the in-memory mock server instead of Laravel.
    ///
    /// Used by `--mock-server` and integration tests to drive the complete
    /// network stack (session driver, handshake, snapshots, prediction)
    /// without an external server.
    pub fn connect_mock(&mut self, config: crate::net::mock::MockServerConfig) {
        self.connect_automatically = false;
        self.connect_with(|net, hello| net.connect_mock(hello, config));
    }

    /// Gives the network a runtime that outlives this call, then connects.
    ///
    /// Only creates one when the caller has no ambient runtime: tests and
    /// other embedders often provide their own (and sometimes expect to drive
    /// it themselves).
    fn connect_with(&mut self, connect: impl FnOnce(&mut GameClient, Hello) -> anyhow::Result<()>) {
        if tokio::runtime::Handle::try_current().is_err() && self.runtime.is_none() {
            // Multi-threaded on purpose: `run_headless` blocks the game thread
            // for the whole run, so a worker thread has to keep polling the
            // socket (the driver creates its connection future inside this
            // runtime's context). The runtime is owned by the app and lives as
            // long as the game loop, so nothing drops it from async context.
            match crate::app::runtime::build_tokio_runtime() {
                Ok(runtime) => self.runtime = Some(runtime),
                Err(err) => {
                    tracing::error!(%err, "failed to start network runtime");
                    self.state.last_error = Some(err.to_string());
                    return;
                }
            }
        }

        let hello = Hello::new(
            self.config.player_name.clone(),
            self.config.auth_token.clone(),
        )
        .with_match(self.config.mode.clone());
        // The runtime lives on this (the game) thread, so it must also be the
        // context the connection factory is created in.
        let result = match &self.runtime {
            Some(runtime) => {
                let _guard = runtime.enter();
                connect(&mut self.net, hello)
            }
            None => connect(&mut self.net, hello),
        };
        self.report_connect(result);
        self.mode_pulled = false;
    }

    /// Saves the package once, and only once the server answered with it.
    ///
    /// `net::client` stores the payload in memory; writing it here keeps file
    /// I/O out of the frame path that runs while the payload arrives.
    fn write_discovered_modes(&mut self) {
        for package in self.net.take_mode_packages() {
            match crate::mode_cache::store(&package) {
                Ok(dir) => {
                    tracing::info!(mode = %package.id, path = %dir.display(), bytes = package.script.len(), "mode package saved");
                    self.notifications.push(format!(
                        "Mode '{}' downloaded → {}",
                        package.id,
                        dir.display()
                    ));
                }
                Err(err) => {
                    tracing::warn!(%err, mode = %package.id, "could not save the mode package");
                    self.notifications
                        .push(format!("Mode download failed: {err}"));
                }
            }
        }
    }

    /// Warns when `--mode` was requested but the server assigned another kind
    /// of match (unknown id, draft, or the server did not read the field).
    fn check_requested_mode(&mut self) {
        let (Some(requested), Some(assigned)) =
            (self.config.mode.as_deref(), self.state.match_id.as_ref())
        else {
            return;
        };
        let assigned = assigned.as_str();

        if requested != assigned {
            tracing::warn!(
                requested,
                assigned,
                "the server did not join the requested mode; playing deathmatch. \
                 Check `php artisan game:mode:owner` / /api/v1/modes that the mode is live"
            );
            self.notifications
                .push(format!("Mode '{requested}' not available – deathmatch"));
            self.state.last_error = Some(format!(
                "Mode '{requested}' not available – server put you in '{assigned}' (deathmatch)"
            ));
        }
    }

    /// Asks the server once per session for the Lua package of the match.
    fn pull_mode_package(&mut self) {
        if self.mode_pulled || !self.net.is_connected() {
            return;
        }

        if self.state.mode.is_none() && self.configured_mode.is_none() {
            // Built-in Deathmatch: there is no package to download.
            return;
        }

        self.mode_pulled = true;
        self.net
            .send_message(crate::protocol::message::ProtocolMessage::Pull(
                crate::protocol::message::Pull::mode(),
            ));
    }

    fn report_connect(&mut self, result: anyhow::Result<()>) {
        if let Err(err) = result {
            tracing::error!(%err, "failed to start network driver");
            self.state.last_error = Some(err.to_string());
        }
    }

    // -----------------------------------------------------------------
    // Frame
    // -----------------------------------------------------------------

    /// One rendered frame.
    ///
    /// Order is deliberate: authoritative data first, then input/prediction,
    /// then presentation.
    pub fn frame(&mut self, frame_time: Duration, input: InputSample) {
        // 1. Apply everything the server told us since the last frame, then
        //    ask for the mode package once the handshake is done.
        let network = self.net.update(&mut self.state);
        self.check_requested_mode();
        self.pull_mode_package();
        self.write_discovered_modes();
        self.notifications.extend(network.notifications);
        self.sync_tick_offset();
        if let Some(local) = self.state.local_player.clone() {
            self.predictor.set_local_player(Some(local));
        }
        self.predictor.set_speed(self.state.move_speed);
        // Reconciliation: take the authoritative base position and drop every
        // input the server has acknowledged. This is the only place the replay
        // buffer shrinks – without it, it overflows at a constant rate and
        // prediction never gets authority.
        if let Some(snapshot) = self.net.take_applied_snapshot() {
            self.predictor
                .on_snapshot(&snapshot, &mut self.input_buffer);
        }
        self.sync_ack();

        // 2. Fixed-tick simulation.
        let steps = self.clock.advance(frame_time);
        for _ in 0..steps {
            self.tick(input.clone());
        }

        // 3. Presentation state.
        self.state.visuals.update(frame_time.as_secs_f32());
        self.update_camera(frame_time);
    }

    /// Follows the local player, snapping instead of sweeping on the first
    /// frame of a session (and after a teleport such as a respawn).
    fn update_camera(&mut self, frame_time: Duration) {
        let Some(target) = self
            .state
            .local_override
            .or_else(|| self.state.local_player().map(|p| p.position))
        else {
            // No player yet: there is nothing meaningful to follow, so do not
            // drift towards a stale target either.
            self.camera_placed = false;
            return;
        };

        // Snap when the target is outside the current view by a wide margin –
        // that is a session start, a reconnect or a respawn, never normal
        // movement (which the follow logic handles continuously).
        let far_away =
            (target - self.camera.center).length() > self.camera.viewport.length().max(64.0);
        // A respawn is a teleport even when it happens to land nearby.
        let alive = self
            .state
            .local_player()
            .map(|player| player.is_alive)
            .unwrap_or(false);
        let respawned = alive && !self.local_was_alive;
        self.local_was_alive = alive;
        if !self.camera_placed || far_away || respawned {
            self.camera.snap_to(target);
            self.camera_placed = true;
        }
        self.camera.follow(
            target,
            frame_time.as_secs_f32(),
            self.state.visuals.screen_shake,
        );
    }

    /// One fixed tick: sample input, send it, predict.
    fn tick(&mut self, input: InputSample) {
        let tick = self.clock.current_tick();
        let tick_rate = TickRate::new(self.state.tick_rate);

        // Sampling is per *tick* (60 Hz), independent of the render rate: the
        // predicted position only advances when a new sample exists, so a
        // lower rate makes the local sprite move in visible steps. Acks can
        // cover several sequences at once, so 60 Hz does not grow the buffer.

        // Nothing is sampled while the handshake is incomplete: every sample
        // would be dropped by the session driver, and filling the replay buffer
        // with 60 unacked inputs per second is exactly what makes a client that
        // started before the server log "input buffer full" forever.
        if !self.net.is_connected() {
            self.input_pipeline.clear_pending_actions();
            self.input_buffer.clear();
            self.predictor.reset();
            self.state.local_override = None;
            return;
        }
        let interval_ticks = self.config.net.input_interval_ticks.max(1);
        if self.input_buffer.take_overflow() > 0 {
            // Only diagnostic on the HUD: on a LAN this should never happen.
            self.state.buffer_overflows = self.state.buffer_overflows.saturating_add(1);
        }

        // Send at most one sample per `interval_ticks`, but always send the
        // tick's movement so the server never sees a gap while connected.
        if tick.0 >= self.next_input_tick {
            self.next_input_tick = tick.0 + interval_ticks;

            // Queue one-shot actions *before* sampling so they leave in this
            // tick's message; `sample` flushes them exactly once.
            for action in input.actions {
                self.input_pipeline.push_action(action);
            }
            let message = self.input_pipeline.sample(tick, input.movement);

            self.input_buffer.push(message.clone());
            self.net.send_input_message(message);
        }

        // Prediction is local-only; the server result overwrites it on arrival.
        let prediction = self
            .predictor
            .predict(tick, &self.input_buffer, tick_rate.hz());
        // `position` already is authoritative base + replayed inputs.
        self.state.local_override = prediction.has_authority.then_some(prediction.position);
    }

    /// Aligns the local clock with the server tick timeline.
    ///
    /// The interpolator renders "in the past" on the *server* tick axis. Using
    /// the local tick here was wrong (it starts at 0 while a running server is
    /// at some large tick): every remote player then fell outside the snapshot
    /// buffer and was pinned to its oldest entry, jumping whenever the buffer
    /// evicted one.
    fn sync_tick_offset(&mut self) {
        let local = self.clock.current_tick().0 as i64;
        let server = self.state.server_tick.0 as i64;
        match self.tick_offset {
            // Re-sync after a reconnect or a long stall (only from real data:
            // `server_tick` is 0 until the first welcome/snapshot).
            Some(offset) if server > 0 && (server - (local + offset)).abs() > 90 => {
                tracing::debug!(old = offset, new = server - local, "resynced tick offset");
                self.tick_offset = Some(server - local);
            }
            Some(_) => {}
            None if server > 0 => self.tick_offset = Some(server - local),
            None => {}
        }
        if !self.state.is_connected() {
            // A new session may get a completely different server tick.
            self.tick_offset = None;
        }
    }

    /// Screen positions of every visible player, exactly as the renderer
    /// computes them this frame.
    ///
    /// Useful for the minimap and for tests that verify motion is smooth
    /// (per-frame deltas in *screen* pixels, not world units).
    pub fn rendered_positions(
        &mut self,
    ) -> std::collections::HashMap<crate::core::PlayerId, crate::core::Vec2> {
        let server_tick = self.estimated_server_tick();
        let ClientApp {
            state, camera, net, ..
        } = self;
        let render_tick = render_tick_of(state, net.interpolator(), server_tick);
        let ids: Vec<_> = state.players.keys().cloned().collect();
        let interpolator = net.interpolator();
        ids.into_iter()
            .filter_map(|id| {
                let position = SceneRenderer::player_screen_position(
                    state,
                    camera,
                    interpolator,
                    render_tick,
                    &id,
                )?;
                Some((id, position))
            })
            .collect()
    }

    /// The renderer's interpolation tick (server timeline, ~half RTT behind).
    pub fn render_tick(&mut self) -> f32 {
        let server_tick = self.estimated_server_tick();
        render_tick_of(&self.state, self.net.interpolator(), server_tick)
    }

    /// The server tick the client believes is current (smooth, per frame).
    fn estimated_server_tick(&self) -> u64 {
        match self.tick_offset {
            Some(offset) => (self.clock.current_tick().0 as i64 + offset).max(0) as u64,
            None => self.state.server_tick.0,
        }
    }

    /// Runs diagnostics that do not belong in the hot path (once per frame).
    ///
    /// Keeps reconnect bookkeeping in one place: after a reconnect the server
    /// ignores inputs whose sequence it has already seen, so the input counter
    /// is moved past the last acknowledged sample and the replay buffer starts
    /// empty.
    fn sync_ack(&mut self) {
        let acked = self.net.effects.last_acked_sequence();
        if acked == self.last_acked_input {
            return;
        }
        self.last_acked_input = acked;
        if let Some(acked) = acked {
            self.input_pipeline.ensure_sequence_after(acked);
        }
    }

    fn dispatch_effects(&mut self, effects: Vec<FxEvent>, backend: &mut dyn AudioBackend) {
        for effect in effects {
            match effect {
                FxEvent::Sound(cue) => self.mixer.play(backend, cue),
                FxEvent::Shake { magnitude } => {
                    self.state.visuals.screen_shake = magnitude;
                }
                FxEvent::Toast(message) => {
                    self.notifications.push(message);
                }
                FxEvent::Particles { .. } => {
                    // Particle systems arrive with the real art pipeline.
                }
            }
        }
    }

    // -----------------------------------------------------------------
    // GUI loop
    // -----------------------------------------------------------------

    pub fn run_gui(&mut self, max_frames: Option<u64>) -> anyhow::Result<ExitReason> {
        let mut platform = DesktopPlatform::open(&self.config.render)?;
        let keyboard = KeyboardInputSource::new(self.config.input.deadzone);
        let mut last_frame = Instant::now();
        let mut frames = 0u64;
        let mut backend = NullAudioBackend::default();

        self.connect_if_needed();

        loop {
            if platform.should_close() {
                return Ok(ExitReason::WindowClosed);
            }
            if let Some(max) = max_frames
                && frames >= max
            {
                return Ok(ExitReason::TickBudgetReached);
            }

            if platform.debug_toggled() {
                self.config.render.show_debug_overlay = !self.config.render.show_debug_overlay;
            }

            let now = Instant::now();
            let frame_time = now - last_frame;
            last_frame = now;

            let sample = keyboard.capture(&platform.rl);
            self.frame(frame_time, sample);

            // --- local effects → audio ------------------------------------
            let fx = self.net.effects.drain();
            self.dispatch_effects(fx, &mut backend);

            // --- render ---------------------------------------------------
            let fps = platform.fps();
            let pending_inputs = self.input_buffer.len();
            let state = &self.state;
            let config = &self.config.render;
            let camera = &self.camera;
            let server_tick = self.estimated_server_tick();
            let interpolator = self.net.interpolator();
            let render_tick = render_tick_of(state, interpolator, server_tick);
            let server_url = self.config.net.connection.url.clone();

            let (width, height) = platform.window_size();
            // `LAN_BASH_TRACE_LUMA=1` logs the mean brightness of what was drawn
            // this frame (diagnostic for flicker reports).
            let trace_luma = std::env::var("LAN_BASH_TRACE_LUMA").is_ok_and(|v| v == "1");

            // Exactly **one** BeginDrawing/EndDrawing per frame. Rendering into
            // the texture and presenting it in two separate `draw` calls used to
            // swap the window buffers twice per frame, which shows up as a
            // bright/dark flicker.
            platform.rl.draw(&platform.thread, |mut handle| {
                {
                    let mut mode = handle
                        .begin_texture_mode(&platform.thread, platform.pipeline.internal_mut());
                    SceneRenderer::draw(&mut mode, state, camera, interpolator, render_tick);
                    HudRenderer::draw(&mut mode, state, config, &server_url, pending_inputs, fps);
                }

                if trace_luma && let Some(luma) = platform.pipeline.mean_luma() {
                    tracing::info!(
                        frame = frames,
                        luma = format!("{luma:.1}"),
                        phase = ?state.phase,
                        players = state.players.len(),
                        shake = format!("{:.1}", state.visuals.screen_shake),
                        "luma"
                    );
                }

                handle.clear_background(Color::BLACK);
                platform.pipeline.present(&mut handle, width, height);
            });

            self.notifications.clear();
            frames += 1;
        }
    }

    // -----------------------------------------------------------------
    // Headless loop
    // -----------------------------------------------------------------

    /// Runs the simulation without a window.
    ///
    /// `max_ticks` limits the run (CI smoke test); `None` runs until
    /// interrupted. Time is simulated in wall-clock steps of 16 ms.
    pub fn run_headless(&mut self, max_ticks: Option<u64>) -> anyhow::Result<ExitReason> {
        let mut source = ScriptedInputSource::new();
        let mut backend = NullAudioBackend::default();
        let frame = Duration::from_millis(16);
        let mut total_ticks = 0u64;

        self.connect_if_needed();
        self.wait_for_session(HEADLESS_CONNECT_TIMEOUT);

        loop {
            let frame_started = Instant::now();
            let input = source.poll();
            let before = self.clock.current_tick().0;
            self.frame(frame, input);
            let advanced = self.clock.current_tick().0 - before;
            total_ticks += advanced;

            let fx = self.net.effects.drain();
            self.dispatch_effects(fx, &mut backend);

            if let Some(max) = max_ticks
                && total_ticks >= max
            {
                return Ok(ExitReason::TickBudgetReached);
            }

            // Sleep the remainder of the frame so the network driver can run.
            // Without a budget (`--headless` alone) the loop must run at
            // *real* speed: it used to spin at full CPU speed in a tight
            // sleep(4ms) loop, i.e. ~3x faster than the server, which made it
            // useless for watching a live match and for smoke tests.
            std::thread::sleep(
                frame
                    .checked_sub(frame_started.elapsed())
                    .unwrap_or(Duration::ZERO),
            );
        }
    }

    fn connect_if_needed(&mut self) {
        if self.connect_automatically {
            self.connect();
        }
    }

    /// Pumps network events until the handshake completed or `timeout` elapsed.
    ///
    /// Headless runs tick the clock as fast as the CPU allows, so without this
    /// they can finish their whole tick budget before the first snapshot
    /// arrives (the session driver needs a few 10s of ms even in memory).
    /// The GUI does not need it: it connects while the window is opening.
    fn wait_for_session(&mut self, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            self.net.update(&mut self.state);
            if self.net.is_connected() {
                self.pull_mode_package();
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        tracing::warn!(
            url = %self.config.net.connection.url,
            timeout = ?timeout,
            "no session established, continuing without snapshots"
        );
    }

    /// The follow camera (read-only), for diagnostics and future UI (minimap).
    pub fn camera(&self) -> &crate::camera::Camera {
        &self.camera
    }

    /// Unacknowledged input samples still in the replay buffer.
    pub fn pending_inputs(&self) -> usize {
        self.input_buffer.len()
    }

    /// Accessor for tests: the local tick counter.
    pub fn current_tick(&self) -> Tick {
        self.clock.current_tick()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Vec2;

    /// A client that never connects: exercises the frame loop offline.
    fn offline_app() -> ClientApp {
        ClientApp::new(ClientConfig::default())
    }

    #[test]
    fn offline_frames_advance_ticks_and_send_nothing() {
        let mut app = offline_app();
        let right = InputSample {
            movement: Vec2::RIGHT,
            actions: vec![],
        };
        for _ in 0..10 {
            app.frame(Duration::from_millis(16), right.clone());
        }
        // ~10 frames × 16 ms ≈ 9-10 ticks at 60 Hz.
        let ticks = app.current_tick().0;
        assert!(ticks >= 8, "expected ticks to advance, got {ticks}");
    }

    #[test]
    fn no_prediction_without_authority() {
        let mut app = offline_app();
        app.frame(Duration::from_millis(16), InputSample::default());
        assert!(app.state().local_override.is_none());
    }
}
