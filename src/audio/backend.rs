use std::collections::HashMap;

use crate::effects::SoundCue;

/// Receives sound requests. Implementations must be cheap and must not panic
/// when an asset is missing.
pub trait AudioBackend {
    /// Plays the cue once. `volume` is 0..1 and already includes the global mix.
    fn play(&mut self, cue: SoundCue, volume: f32);

    /// Updates listenter/looping sounds. Called once per frame.
    fn update(&mut self, _frame_time: f32) {}

    /// Master volume change (0..1).
    fn set_master_volume(&mut self, _volume: f32) {}
}

/// Discards every cue. Used in `--headless` and unit tests.
#[derive(Debug, Default, Clone)]
pub struct NullAudioBackend {
    played: Vec<SoundCue>,
}

impl NullAudioBackend {
    pub fn played(&self) -> &[SoundCue] {
        &self.played
    }
}

impl AudioBackend for NullAudioBackend {
    fn play(&mut self, cue: SoundCue, _volume: f32) {
        self.played.push(cue);
    }
}

/// Maps sound cues to asset files. Central so renaming a file is one edit.
pub fn asset_path(cue: SoundCue) -> &'static str {
    match cue {
        SoundCue::Hit => "assets/audio/hit.wav",
        SoundCue::HitLocal => "assets/audio/hit_local.wav",
        SoundCue::PlayerJoined => "assets/audio/join.wav",
        SoundCue::PlayerLeft => "assets/audio/leave.wav",
        SoundCue::RoundStart => "assets/audio/round_start.wav",
        SoundCue::RoundEnd => "assets/audio/round_end.wav",
        SoundCue::Countdown => "assets/audio/countdown.wav",
        SoundCue::ItemPickup => "assets/audio/pickup.wav",
    }
}

/// Per-cue volume trims, so a single `master_volume` still keeps the mix sane.
pub fn cue_gain(cue: SoundCue) -> f32 {
    match cue {
        SoundCue::HitLocal => 0.9,
        SoundCue::Hit => 0.6,
        SoundCue::Countdown | SoundCue::RoundStart | SoundCue::RoundEnd => 0.7,
        _ => 0.5,
    }
}

/// Helper used by backends to remember which cues were requested this frame.
#[derive(Debug, Default)]
pub struct CueLog {
    counts: HashMap<&'static str, u32>,
}

impl CueLog {
    pub fn record(&mut self, cue: SoundCue) {
        *self.counts.entry(asset_path(cue)).or_insert(0) += 1;
    }

    pub fn take(&mut self) -> HashMap<&'static str, u32> {
        std::mem::take(&mut self.counts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_cue_has_a_gain_in_range() {
        for cue in [
            SoundCue::Hit,
            SoundCue::HitLocal,
            SoundCue::PlayerJoined,
            SoundCue::PlayerLeft,
            SoundCue::RoundStart,
            SoundCue::RoundEnd,
            SoundCue::Countdown,
            SoundCue::ItemPickup,
        ] {
            let gain = cue_gain(cue);
            assert!((0.0..=1.0).contains(&gain));
            assert!(asset_path(cue).starts_with("assets/audio/"));
        }
    }

    #[test]
    fn cue_log_counts() {
        let mut log = CueLog::default();
        log.record(SoundCue::Hit);
        log.record(SoundCue::Hit);
        let counts = log.take();
        assert_eq!(counts["assets/audio/hit.wav"], 2);
        assert!(log.take().is_empty());
    }
}
