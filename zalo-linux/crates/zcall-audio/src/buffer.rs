//! A small ring buffer of i16 PCM samples shared between capture and playout.

use std::collections::VecDeque;

/// A bounded FIFO of i16 samples. When full, the oldest samples are dropped.
#[derive(Debug)]
pub struct FrameBuffer {
    samples: VecDeque<i16>,
    capacity: usize,
}

impl FrameBuffer {
    /// Builds a buffer holding at most `capacity_samples` samples.
    pub fn new(capacity_samples: usize) -> Self {
        Self {
            samples: VecDeque::new(),
            capacity: capacity_samples.max(1),
        }
    }

    /// Appends samples, dropping the oldest when the buffer is full.
    pub fn push(&mut self, pcm: &[i16]) {
        for &sample in pcm {
            if self.samples.len() == self.capacity {
                self.samples.pop_front();
            }
            self.samples.push_back(sample);
        }
    }

    /// Removes up to `out.len()` samples, returning the count written.
    pub fn pop(&mut self, out: &mut [i16]) -> usize {
        let count = out.len().min(self.samples.len());
        for slot in out.iter_mut().take(count) {
            *slot = self.samples.pop_front().expect("buffered sample");
        }
        count
    }

    /// Number of samples currently buffered.
    pub fn available(&self) -> usize {
        self.samples.len()
    }

    /// Whether the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// Drops everything.
    pub fn clear(&mut self) {
        self.samples.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_and_pop_round_trip() {
        let mut buf = FrameBuffer::new(1024);
        buf.push(&[1, 2, 3, 4, 5]);
        let mut out = [0_i16; 8];
        let n = buf.pop(&mut out);
        assert_eq!(n, 5);
        assert_eq!(&out[..5], &[1, 2, 3, 4, 5]);
        assert!(buf.is_empty());
    }

    #[test]
    fn overflows_by_dropping_oldest() {
        let mut buf = FrameBuffer::new(4);
        buf.push(&[1, 2, 3, 4, 5, 6]);
        assert_eq!(buf.available(), 4);
        let mut out = [0_i16; 4];
        let n = buf.pop(&mut out);
        assert_eq!(n, 4);
        assert_eq!(out, [3, 4, 5, 6]);
    }

    #[test]
    fn clear_drops_everything() {
        let mut buf = FrameBuffer::new(8);
        buf.push(&[1, 2, 3]);
        buf.clear();
        assert_eq!(buf.available(), 0);
        assert!(buf.is_empty());
    }
}
