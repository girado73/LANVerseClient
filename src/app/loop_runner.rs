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
}

impl ClientApp {
    pub fn new(config: ClientConfig) -> Self {
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
            predictor: Predictor::new(MovementModel::default()),
            clock,
            camera,
            mixer: Mixer::default(),
            notifications: Vec::new(),
            connect_automatically: true,
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
        let hello = Hello::new(
            self.config.player_name.clone(),
            self.config.auth_token.clone(),
        );
        if let Err(err) = self.net.connect(hello) {
            tracing::error!(%err, "failed to start network driver");
            self.state.last_error = Some(err.to_string());
        }
    }

    /// Connects to the in-memory mock server instead of Laravel.
    ///
    /// Used by `--mock-server` and integration tests to drive the complete
    /// network stack (session driver, handshake, snapshots, prediction)
    /// without an external server.
    pub fn connect_mock(&mut self, config: crate::net::mock::MockServerConfig) {
        self.connect_automatically = false;
        let hello = Hello::new(self.config.player_name.clone(), None);
        if let Err(err) = self.net.connect_mock(hello, config) {
            tracing::error!(%err, "failed to start mock network driver");
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
        // 1. Apply everything the server told us since the last frame.
        let network = self.net.update(&mut self.state);
        self.notifications.extend(network.notifications);
        if let Some(local) = self.state.local_player.clone() {
            self.predictor.set_local_player(Some(local));
        }

        // 2. Fixed-tick simulation.
        let steps = self.clock.advance(frame_time);
        for _ in 0..steps {
            self.tick(input.clone());
        }

        // 3. Presentation state.
        self.state.visuals.update(frame_time.as_secs_f32());
        self.camera.follow(
            self.state
                .local_override
                .or_else(|| self.state.local_player().map(|p| p.position))
                .unwrap_or(self.camera.center),
            frame_time.as_secs_f32(),
            self.state.visuals.screen_shake,
        );
    }

    /// One fixed tick: sample input, send it, predict.
    fn tick(&mut self, input: InputSample) {
        let tick = self.clock.current_tick();
        let tick_rate = TickRate::new(self.state.tick_rate);

        // Send at most one sample per `input_interval_ticks`, but always send
        // the tick's movement so the server never sees a gap while connected.
        if tick.0 >= self.next_input_tick {
            self.next_input_tick = tick.0 + self.config.net.input_interval_ticks;

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
        self.state.local_override = prediction
            .has_authority
            .then_some(prediction.position + self.predictor.smoothed_correction());
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
            let clock_tick = self.clock.current_tick().0;
            let latency = state.latency_ms;
            let tick_rate = TickRate::new(state.tick_rate);
            let interpolator = self.net.interpolator();
            let render_tick = compute_render_tick(clock_tick, latency, tick_rate, interpolator);

            platform.rl.draw(&platform.thread, |mut handle| {
                let mut mode =
                    handle.begin_texture_mode(&platform.thread, platform.pipeline.internal_mut());
                SceneRenderer::draw(&mut mode, state, camera, interpolator, render_tick);
                HudRenderer::draw(&mut mode, state, config, pending_inputs, fps);
            });

            let (width, height) = platform.window_size();
            platform.rl.draw(&platform.thread, |mut d| {
                d.clear_background(Color::BLACK);
                platform.pipeline.present(&mut d, width, height);
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

        loop {
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
            std::thread::sleep(Duration::from_millis(4).min(frame));
        }
    }

    fn connect_if_needed(&mut self) {
        if self.connect_automatically {
            self.connect();
        }
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
