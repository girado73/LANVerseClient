//! raylib-backed window and keyboard input.

use raylib::consts::KeyboardKey;
use raylib::prelude::*;

use crate::config::RenderConfig;
use crate::core::Vec2;
use crate::input::InputActions;
use crate::input::source::InputSample;
use crate::render::RenderPipeline;

/// Keyboard layout for the prototype:
///
/// | input          | keys                     |
/// |----------------|--------------------------|
/// | move           | WASD / arrow keys        |
/// | jump           | Space                    |
/// | dash           | Left Shift               |
/// | interact       | E                        |
/// | use item       | F                        |
///
/// Input is captured as *digital intent*, never as position: the server decides
/// how far a player moves. Edge-triggered actions come from raylib's
/// `is_key_pressed`, so holding a key does not repeat an action.
pub struct KeyboardInputSource {
    deadzone: f32,
}

impl KeyboardInputSource {
    pub fn new(deadzone: f32) -> Self {
        Self {
            deadzone: deadzone.clamp(0.0, 1.0),
        }
    }

    /// Samples one fixed tick. Called by the app, which owns the raylib handle.
    pub fn capture(&self, rl: &RaylibHandle) -> InputSample {
        let mut movement = Vec2::ZERO;
        if rl.is_key_down(KeyboardKey::KEY_A) || rl.is_key_down(KeyboardKey::KEY_LEFT) {
            movement.x -= 1.0;
        }
        if rl.is_key_down(KeyboardKey::KEY_D) || rl.is_key_down(KeyboardKey::KEY_RIGHT) {
            movement.x += 1.0;
        }
        if rl.is_key_down(KeyboardKey::KEY_W) || rl.is_key_down(KeyboardKey::KEY_UP) {
            movement.y -= 1.0;
        }
        if rl.is_key_down(KeyboardKey::KEY_S) || rl.is_key_down(KeyboardKey::KEY_DOWN) {
            movement.y += 1.0;
        }

        if movement.length() < self.deadzone {
            movement = Vec2::ZERO;
        }

        let mut actions = Vec::new();
        if rl.is_key_pressed(KeyboardKey::KEY_SPACE) {
            actions.push(InputActions::Jump);
        }
        if rl.is_key_pressed(KeyboardKey::KEY_LEFT_SHIFT) {
            actions.push(InputActions::Dash);
        }
        if rl.is_key_pressed(KeyboardKey::KEY_E) {
            actions.push(InputActions::Interact);
        }
        if rl.is_key_pressed(KeyboardKey::KEY_F) {
            actions.push(InputActions::UseItem);
        }

        InputSample { movement, actions }
    }
}

/// Owns the raylib window and the pixelart render pipeline.
pub struct DesktopPlatform {
    pub rl: RaylibHandle,
    pub thread: RaylibThread,
    pub pipeline: RenderPipeline,
}

impl DesktopPlatform {
    pub fn open(config: &RenderConfig) -> anyhow::Result<Self> {
        let (mut rl, thread) = raylib::init()
            .size(config.window_width, config.window_height)
            .title(&config.title)
            .resizable()
            .vsync()
            .build();
        rl.set_target_fps(config.target_fps);

        let pipeline = RenderPipeline::new(&mut rl, &thread, config)?;
        Ok(Self {
            rl,
            thread,
            pipeline,
        })
    }

    /// True when the user closed the window.
    pub fn should_close(&self) -> bool {
        self.rl.window_should_close()
    }

    pub fn window_size(&self) -> (i32, i32) {
        (self.rl.get_screen_width(), self.rl.get_screen_height())
    }

    pub fn frame_time(&self) -> f32 {
        self.rl.get_frame_time()
    }

    pub fn fps(&self) -> u32 {
        self.rl.get_fps()
    }

    /// `true` when the user pressed F1 this frame (toggle debug overlay).
    pub fn debug_toggled(&self) -> bool {
        self.rl.is_key_pressed(KeyboardKey::KEY_F1)
    }
}
