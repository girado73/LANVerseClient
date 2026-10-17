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
}

impl InputBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            pending: VecDeque::with_capacity(capacity.max(1)),
        }
    }

    /// Stores a sampled input. If the buffer is full the **oldest** sample is
    /// dropped: it is the one the server has most likely already consumed.
    pub fn push(&mut self, input: InputMessage) {
        if self.pending.len() >= self.capacity {
            self.pending.pop_front();
            tracing::debug!("input buffer full, dropping oldest unacked sample");
        }
        self.pending.push_back(input);
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
        before - self.pending.len()
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
    }
}
