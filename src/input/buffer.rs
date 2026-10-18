//! Bounded history of sampled inputs.
//!
//! The buffer is the basis for client-side prediction and reconciliation:
//! when the server acks sequence `N`, every sample up to and including `N`
//! is removed and the remaining ones are replayed on top of the authoritative
//! state by `prediction::Predictor`.

use std::collections::VecDeque;

use crate::protocol::message::InputMessage;

#[derive(Debug)]
pub struct InputBuffer {
    capacity: usize,
    pending: VecDeque<InputMessage>,
    /// Samples dropped because of overflow since the last [`InputBuffer::take_overflow`].
    overflowed: u64,
    /// Whether overflow has already been reported, so the log line is emitted
    /// once per overflow streak instead of once per tick (60/s is useless noise).
    overflow_logged: bool,
}

impl InputBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            pending: VecDeque::with_capacity(capacity.max(1)),
            overflowed: 0,
            overflow_logged: false,
        }
    }

    /// Stores a sampled input. If the buffer is full the **oldest** sample is
    /// dropped: it is the one the server has most likely already consumed.
    ///
    /// Overflow means the server is acking slower than the client samples, i.e.
    /// prediction is degraded (not wrong – the server stays authoritative). It
    /// is counted and logged **once per streak**; the streak ends as soon as an
    /// ack drains the buffer again, so a new overflow logs again.
    pub fn push(&mut self, input: InputMessage) {
        if self.pending.len() >= self.capacity {
            self.pending.pop_front();
            self.overflowed += 1;
            if !self.overflow_logged {
                self.overflow_logged = true;
                tracing::debug!(
                    capacity = self.capacity,
                    "input buffer full, dropping oldest unacked samples (further drops are counted)"
                );
            }
        }
        self.pending.push_back(input);
    }

    /// Returns (and resets) the number of samples dropped since the last call.
    ///
    /// Deliberately does *not* reset the log flag: it is polled every tick, and
    /// that used to re-arm the "once per streak" logging on every tick.
    pub fn take_overflow(&mut self) -> u64 {
        std::mem::take(&mut self.overflowed)
    }

    /// Removes everything up to and including `acked_sequence`.
    /// Returns how many samples were discarded.
    pub fn acknowledge_through(&mut self, acked_sequence: u64) -> usize {
        let before = self.pending.len();
        while let Some(front) = self.pending.front() {
            if front.sequence <= acked_sequence {
                self.pending.pop_front();
            } else {
                break;
            }
        }
        let removed = before - self.pending.len();
        if removed > 0 && self.pending.len() < self.capacity {
            // The buffer drained – a later overflow is a new event.
            self.overflow_logged = false;
        }
        removed
    }

    /// All inputs that still need to be replayed, oldest first.
    pub fn pending(&self) -> impl Iterator<Item = &InputMessage> {
        self.pending.iter()
    }

    pub fn len(&self) -> usize {
        self.pending.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    pub fn clear(&mut self) {
        self.pending.clear();
        self.overflowed = 0;
        self.overflow_logged = false;
    }

    /// Highest sequence ever sampled (for diagnostics).
    pub fn newest_sequence(&self) -> Option<u64> {
        self.pending.back().map(|i| i.sequence)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Vec2;

    fn input(sequence: u64) -> InputMessage {
        InputMessage {
            client_tick: sequence,
            sequence,
            movement: Vec2::RIGHT,
            actions: vec![],
        }
    }

    #[test]
    fn acknowledge_removes_prefix_only() {
        let mut buffer = InputBuffer::new(16);
        for s in 1..=5 {
            buffer.push(input(s));
        }
        assert_eq!(buffer.acknowledge_through(3), 3);
        let remaining: Vec<_> = buffer.pending().map(|i| i.sequence).collect();
        assert_eq!(remaining, [4, 5]);
        // Acking a sequence we never stored is harmless.
        assert_eq!(buffer.acknowledge_through(100), 2);
        assert!(buffer.is_empty());
    }

    #[test]
    fn capacity_drops_oldest() {
        let mut buffer = InputBuffer::new(2);
        for s in 1..=3 {
            buffer.push(input(s));
        }
        let remaining: Vec<_> = buffer.pending().map(|i| i.sequence).collect();
        assert_eq!(remaining, [2, 3]);
        assert_eq!(buffer.newest_sequence(), Some(3));
        // The drop is counted exactly once and the counter resets on read.
        assert_eq!(buffer.take_overflow(), 1);
        assert_eq!(buffer.take_overflow(), 0);
    }

    #[test]
    fn acknowledging_drains_and_rearms_overflow_reporting() {
        let mut buffer = InputBuffer::new(2);
        for s in 1..=3 {
            buffer.push(input(s));
        }
        assert_eq!(buffer.take_overflow(), 1);

        // Acking everything shrinks the buffer below capacity again.
        assert_eq!(buffer.acknowledge_through(3), 2);
        assert!(buffer.is_empty());

        // A later overflow is a new streak and must be observable again.
        for s in 4..=6 {
            buffer.push(input(s));
        }
        assert_eq!(buffer.take_overflow(), 1);
    }
}
