//! Platform integration.
//!
//! This layer adapts *the outside world* (window, keyboard, clock) to the
//! traits the game logic consumes:
//!
//! * `platform::desktop::KeyboardInputSource` implements
//!   [`crate::input::source::InputSource`] using raylib's input API.
//! * `platform::desktop::DesktopWindow` owns the raylib handle/thread and the
//!   [`RenderPipeline`](crate::render::RenderPipeline).
//!
//! Swapping raylib for another backend means implementing these two things;
//! `input`, `simulation`, `net`, `models` remain untouched.

pub mod desktop;

pub use desktop::{DesktopPlatform, KeyboardInputSource};
