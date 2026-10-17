//! Volume management and rate-limiting for sound cues.
//!
//! A server burst (10 players hit in the same frame) would produce a wall of
//! overlapping samples. The mixer keeps a per-cue cooldown so identical sounds
//! within a few milliseconds collapse into one.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::audio::backend::{AudioBackend, cue_gain};
use crate::effects::SoundCue;

/// Minimum time between two plays of the same cue.
const DEFAULT_COOLDOWN_MS: u64 = 30;

#[derive(Debug)]
pub struct Mixer {
    master_volume: f32,
    cooldowns: HashMap<SoundCue, Instant>,
    cooldown: Duration,
}

impl Default for Mixer {
    fn default() -> Self {
        Self::new(0.8)
    }
}

impl Mixer {
    pub fn new(master_volume: f32) -> Self {
        Self {
            master_volume: master_volume.clamp(0.0, 1.0),
            cooldowns: HashMap::new(),
            cooldown: Duration::from_millis(DEFAULT_COOLDOWN_MS),
        }
    }

    pub fn master_volume(&self) -> f32 {
        self.master_volume
    }

    pub fn set_master_volume(&mut self, volume: f32) {
        self.master_volume = volume.clamp(0.0, 1.0);
    }

    /// Plays `cue` on `backend` unless muted or cooling down.
    pub fn play(&mut self, backend: &mut dyn AudioBackend, cue: SoundCue) {
        if self.master_volume <= 0.0 {
            return;
        }

        let now = Instant::now();
        if let Some(last) = self.cooldowns.get(&cue)
            && now.duration_since(*last) < self.cooldown
        {
            return;
        }
        self.cooldowns.insert(cue, now);
        backend.play(cue, self.master_volume * cue_gain(cue));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::backend::NullAudioBackend;

    #[test]
    fn repeated_cue_within_cooldown_is_dropped() {
        let mut mixer = Mixer::new(1.0);
        let mut backend = NullAudioBackend::default();

        mixer.play(&mut backend, SoundCue::Hit);
        mixer.play(&mut backend, SoundCue::Hit);
        assert_eq!(backend.played().len(), 1);

        // A different cue is independent.
        mixer.play(&mut backend, SoundCue::RoundStart);
        assert_eq!(backend.played().len(), 2);
    }

    #[test]
    fn muted_mixer_plays_nothing() {
        let mut mixer = Mixer::new(0.0);
        let mut backend = NullAudioBackend::default();
        mixer.play(&mut backend, SoundCue::Hit);
        assert!(backend.played().is_empty());
    }
}
