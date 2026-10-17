//! Audio playback.
//!
//! The game logic only produces [`SoundCue`]s (see `effects`); the audio
//! backend decides how they sound. [`AudioBackend`] is a trait so that
//!
//! * the raylib implementation can be swapped for a null backend in headless
//!   mode or tests,
//! * future backends (e.g. positional audio, kira) do not touch game logic.
//!
//! Sound assets live under `assets/audio/` and are **optional**: a missing
//! file logs a warning and plays nothing, which keeps the prototype runnable
//! before any art/sound production has happened.

pub mod backend;
pub mod mixer;

pub use backend::{AudioBackend, NullAudioBackend};
pub use mixer::Mixer;
