//! Snapshot interpolation.
//!
//! Snapshots arrive at a lower (and jittery) rate than the renderer draws.
//! Instead of drawing the newest snapshot (which stutters) or extrapolating
//! (which lies about the server state), the client renders **in the past** by
//! roughly half the RTT: it holds a short buffer of snapshots and blends
//! between the two wrapping the current render time.
//!
//! This is purely cosmetic. `models::GameState.players` always contains the
//! latest authoritative values; the interpolator only produces positions for
//! drawing.

use std::collections::VecDeque;

use crate::core::Vec2;
use crate::protocol::message::Snapshot;
use crate::simulation::tick_rate::TickRate;

/// A position produced for rendering at a fractional tick.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InterpolatedPose {
    pub position: Vec2,
    /// 0 = previous snapshot, 1 = next snapshot.
    pub blend: f32,
}

#[derive(Debug)]
pub struct Interpolator {
    buffer: VecDeque<Snapshot>,
    capacity: usize,
    /// Smoothed real-time distance between two snapshots, in ticks.
    snapshot_interval_ticks: f32,
}

impl Interpolator {
    pub fn new(capacity: usize) -> Self {
        Self {
            buffer: VecDeque::with_capacity(capacity.max(2)),
            capacity: capacity.max(2),
            snapshot_interval_ticks: 0.0,
        }
    }

    pub fn clear(&mut self) {
        self.buffer.clear();
        self.snapshot_interval_ticks = 0.0;
    }

    pub fn len(&self) -> usize {
        self.buffer.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    /// Inserts a snapshot keeping the buffer ordered by tick.
    pub fn push(&mut self, snapshot: Snapshot) {
        if let Some(back) = self.buffer.back() {
            if snapshot.tick <= back.tick {
                // A duplicate or an older snapshot – keep the buffer monotonic.
                return;
            }
            let delta = (snapshot.tick - back.tick) as f32;
            self.snapshot_interval_ticks = if self.snapshot_interval_ticks == 0.0 {
                delta
            } else {
                self.snapshot_interval_ticks * 0.8 + delta * 0.2
            };
        }

        self.buffer.push_back(snapshot);
        while self.buffer.len() > self.capacity {
            self.buffer.pop_front();
        }
    }

    /// The tick we should render at.
    ///
    /// `current_tick` is our local prediction tick. We subtract the estimated
    /// one-way latency plus a full snapshot interval so that both snapshots
    /// bracketing the render time are already present.
    pub fn render_tick(&self, current_tick: u64, latency_ms: f32, tick_rate: TickRate) -> f32 {
        let delay_ticks = tick_rate.ms_to_ticks(latency_ms) * 0.5 + self.snapshot_interval_ticks;
        (current_tick as f32 - delay_ticks).max(0.0)
    }

    /// Interpolated pose for `player_id` at `render_tick`.
    ///
    /// * older than the buffer → first snapshot's position
    /// * newer than the buffer → newest snapshot's position (no extrapolation)
    pub fn pose(
        &self,
        player_id: &crate::core::PlayerId,
        render_tick: f32,
    ) -> Option<InterpolatedPose> {
        if self.buffer.is_empty() {
            return None;
        }

        let mut previous: Option<(&Snapshot, Vec2)> = None;
        for snapshot in &self.buffer {
            let tick = snapshot.tick as f32;
            if tick > render_tick {
                let next_pos = position_in(snapshot, player_id);
                return match (previous, next_pos) {
                    (Some((prev_snapshot, prev_pos)), Some(next_pos)) => {
                        let span = tick - prev_snapshot.tick as f32;
                        let blend = if span > 0.0 {
                            ((render_tick - prev_snapshot.tick as f32) / span).clamp(0.0, 1.0)
                        } else {
                            1.0
                        };
                        Some(InterpolatedPose {
                            position: prev_pos.lerp(next_pos, blend),
                            blend,
                        })
                    }
                    // Player only exists in the next snapshot: pop in at its position.
                    (_, Some(next_pos)) => Some(InterpolatedPose {
                        position: next_pos,
                        blend: 1.0,
                    }),
                    // Player disappeared in the next snapshot: keep last known.
                    (Some((_, prev_pos)), None) => Some(InterpolatedPose {
                        position: prev_pos,
                        blend: 1.0,
                    }),
                    (None, None) => None,
                };
            }
            if let Some(pos) = position_in(snapshot, player_id) {
                previous = Some((snapshot, pos));
            }
        }

        // Render tick is ahead of everything we have – clamp to the newest sample.
        previous.map(|(_, position)| InterpolatedPose {
            position,
            blend: 1.0,
        })
    }

    /// Newest snapshot tick currently buffered.
    pub fn newest_tick(&self) -> Option<u64> {
        self.buffer.back().map(|s| s.tick)
    }
}

fn position_in(snapshot: &Snapshot, player_id: &crate::core::PlayerId) -> Option<Vec2> {
    snapshot
        .players
        .iter()
        .find(|player| &player.id == player_id)
        .map(|player| player.position)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::PlayerId;
    use crate::models::world::PlayerState;

    fn snapshot(tick: u64, x: f32) -> Snapshot {
        Snapshot {
            tick,
            players: vec![PlayerState {
                id: PlayerId::from("p"),
                position: Vec2::new(x, 0.0),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn blends_between_two_snapshots() {
        let mut interp = Interpolator::new(8);
        interp.push(snapshot(10, 0.0));
        interp.push(snapshot(20, 100.0));

        let pose = interp.pose(&PlayerId::from("p"), 15.0).unwrap();
        assert!((pose.position.x - 50.0).abs() < 1e-3);
        assert!((pose.blend - 0.5).abs() < 1e-3);
    }

    #[test]
    fn clamps_to_newest_without_extrapolating() {
        let mut interp = Interpolator::new(8);
        interp.push(snapshot(10, 0.0));
        interp.push(snapshot(20, 100.0));

        let pose = interp.pose(&PlayerId::from("p"), 999.0).unwrap();
        assert_eq!(pose.position.x, 100.0);
    }

    #[test]
    fn ignores_duplicate_or_older_snapshots() {
        let mut interp = Interpolator::new(8);
        interp.push(snapshot(10, 0.0));
        interp.push(snapshot(10, 5.0));
        interp.push(snapshot(5, 5.0));
        assert_eq!(interp.len(), 1);
        assert_eq!(interp.newest_tick(), Some(10));
    }

    #[test]
    fn unknown_player_has_no_pose() {
        let mut interp = Interpolator::new(8);
        interp.push(snapshot(10, 0.0));
        assert!(interp.pose(&PlayerId::from("ghost"), 10.0).is_none());
    }

    #[test]
    fn capacity_evicts_oldest() {
        let mut interp = Interpolator::new(2);
        for tick in 1..=5 {
            interp.push(snapshot(tick, tick as f32));
        }
        assert_eq!(interp.len(), 2);
        assert_eq!(interp.newest_tick(), Some(5));
    }
}
