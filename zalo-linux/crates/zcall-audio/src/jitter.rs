//! The receive-side jitter buffer (`_Stream`), ported from the reference
//! call implementation (`audio-io.py`).
//!
//! It orders Opus packets by sequence, absorbs arrival jitter, decodes ahead,
//! conceals loss with FEC/PLC, and shortens a buffer that grew too deep with
//! one pitch period (WSOLA). Pure logic; the Opus decode and WSOLA helpers
//! come from the other modules.

use crate::wsola::{rms, wsola_shrink, FRAME_SAMPLES};
use std::collections::{BTreeMap, VecDeque};
use zcall_core::audio::OpusDecoder;

const MIN_TARGET: usize = 3;
const START_TARGET: usize = 3;
const MAX_EXPAND: u32 = 5;
const WINDOW: usize = 250;
/// Sequence jump large enough to indicate sender stream reset or wrap.
const RESYNC_GAP: u32 = 100;

/// Counters for the playout diagnostics.
#[derive(Clone, Debug, Default)]
pub struct Stats {
    /// Frames decoded successfully.
    pub opus_ok: u64,
    /// Decode failures.
    pub opus_fail: u64,
    /// Packets declared lost.
    pub lost: u64,
    /// Losses covered by in-band FEC.
    pub fec: u64,
    /// Losses covered by packet loss concealment.
    pub plc: u64,
    /// Frames pulled before the buffer was ready.
    pub underrun: u64,
    /// Frames shortened by one pitch period.
    pub accelerate: u64,
    /// Buffer resets.
    pub reset: u64,
    /// Packets that arrived after their play time.
    pub late: u64,
    /// Duplicate packets.
    pub dup: u64,
}

/// A single media stream's jitter buffer.
pub struct JitterBuffer {
    decoder: OpusDecoder,
    packets: BTreeMap<u32, Vec<u8>>,
    next: Option<u32>,
    playing: bool,
    expand: u32,
    carry: Vec<i16>,
    transit: VecDeque<f64>,
    target: usize,
    min_target: usize,
    max_target: usize,
    since_drop: u32,
    last: f64,
    /// Diagnostics.
    pub stats: Stats,
}

impl JitterBuffer {
    /// Builds a buffer for one source.
    ///
    /// # Errors
    ///
    /// Returns the Opus error when the decoder cannot be created.
    pub fn new(
        max_target: usize,
        min_target: Option<usize>,
    ) -> Result<Self, zcall_core::audio::OpusError> {
        let min_target = 2_usize.max(min_target.unwrap_or(MIN_TARGET));
        let max_target = min_target.max(max_target);
        Ok(Self {
            decoder: OpusDecoder::new()?,
            packets: BTreeMap::new(),
            next: None,
            playing: false,
            expand: 0,
            carry: Vec::new(),
            transit: VecDeque::new(),
            target: START_TARGET.max(min_target),
            min_target,
            max_target,
            since_drop: 0,
            last: 0.0,
            stats: Stats::default(),
        })
    }

    /// Updates the floor and ceiling (20 ms frames) from `zrtc_config`.
    ///
    /// Electron applies `minAudioDelayMs` / `maxAudioDelayMs` after the call
    /// is negotiated, which is after this buffer already exists.
    pub fn set_limits(&mut self, max_target: usize, min_target: usize) {
        let min_target = 2.max(min_target);
        let max_target = min_target.max(max_target);
        self.min_target = min_target;
        self.max_target = max_target;
        self.target = self.target.clamp(self.min_target, self.max_target);
    }

    /// The arrival time of the most recent packet.
    pub fn last(&self) -> f64 {
        self.last
    }

    /// Whether there is nothing buffered and no decoded carry left.
    pub fn is_idle(&self) -> bool {
        self.packets.is_empty() && self.carry.is_empty()
    }

    /// Adds one Opus packet with its sequence index and arrival time.
    pub fn push(&mut self, idx: u32, payload: &[u8], now: f64) {
        self.last = now;
        if let Some(next) = self.next {
            let diff = idx.abs_diff(next);
            if diff > RESYNC_GAP {
                // Sender reset sequence numbers (e.g. peer handoff / network switch):
                // re-anchor cleanly instead of dropping the rest of the call as late.
                self.packets.clear();
                self.carry.clear();
                self.transit.clear();
                self.next = Some(idx);
                self.playing = false;
                self.expand = 0;
                self.stats.reset += 1;
            } else if idx < next {
                self.stats.late += 1;
                return;
            }
        }
        if self.packets.contains_key(&idx) {
            self.stats.dup += 1;
            return;
        }
        self.packets.insert(idx, payload.to_vec());
        self.transit.push_back(now - f64::from(idx) * 0.02);
        while self.transit.len() > WINDOW {
            self.transit.pop_front();
        }
        if self.transit.len() >= 20 {
            let mut sorted: Vec<f64> = self.transit.iter().copied().collect();
            sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let hi = (sorted.len() as f64 * 0.95) as usize - 1;
            let spread = sorted[hi] - sorted[0];
            let frames = (spread / 0.02).ceil() as usize + 1;
            self.target = self.max_target.min(self.min_target.max(frames));
        }
    }

    /// Pulls the next 20 ms of PCM, or `None` when the stream is not playing.
    pub fn pull(&mut self) -> Option<Vec<i16>> {
        if !self.playing {
            if self.carry.is_empty() && self.packets.len() < self.target {
                return None;
            }
            self.playing = true;
            if let Some(first) = self.packets.keys().next().copied() {
                if self.next.is_none() || first > self.next.unwrap() {
                    self.next = Some(first);
                }
            }
        }
        self.since_drop += 1;

        let span = self.span();
        if span > self.target + 12 {
            let _ = self.discard_to(self.target);
        } else if span > self.target + 2
            && self.since_drop >= 6
            && self.ensure(FRAME_SAMPLES * 2)
        {
            let head = self.carry.len().min(FRAME_SAMPLES);
            // Prefer time-compressing quiet pauses, but also allow smooth WSOLA
            // acceleration during continuous speech once delay grows past target+5
            // so we never hit a hard `discard_to` cut in the middle of a sentence.
            if rms(&self.carry[..head]) < 900.0 || span > self.target + 5 {
                self.since_drop = 0;
                self.stats.accelerate += 1;
                let (played, rest) = wsola_shrink(&self.carry);
                self.carry = rest;
                return Some(played);
            }
        }
        if self.ensure(FRAME_SAMPLES) {
            return Some(self.take_play());
        }

        let has_packets = !self.packets.is_empty();
        let buffered = self.packets.len() >= self.target || self.expand >= MAX_EXPAND;
        if has_packets && buffered {
            self.expand = 0;
            self.stats.lost += 1;
            let next = self.next.unwrap_or(0);
            let next_payload = self.packets.get(&next.wrapping_add(1)).cloned();
            self.next = Some(next.wrapping_add(1));
            if let Some(payload) = next_payload {
                self.stats.fec += 1;
                let decoded = self.decode(Some(&payload), true);
                return Some(self.emit(decoded));
            }
            self.stats.plc += 1;
            let decoded = self.decode(None, false);
            return Some(self.emit(decoded));
        }

        self.stats.underrun += 1;
        if self.expand < MAX_EXPAND {
            self.expand += 1;
            self.stats.plc += 1;
            let decoded = self.decode(None, false);
            return Some(self.emit(decoded));
        }
        self.expand = 0;
        self.playing = false;
        None
    }

    fn span(&self) -> usize {
        match (self.packets.keys().next_back(), self.next) {
            (Some(last), Some(next)) => {
                let d = i64::from(*last) - i64::from(next) + 1;
                d.max(0) as usize
            }
            _ => 0,
        }
    }

    fn ensure(&mut self, n_samples: usize) -> bool {
        while self.carry.len() < n_samples {
            let Some(next) = self.next else {
                break;
            };
            let Some(payload) = self.packets.remove(&next) else {
                break;
            };
            let decoded = self.decode(Some(&payload), false);
            self.carry.extend_from_slice(&decoded);
            self.next = Some(next.wrapping_add(1));
            self.expand = 0;
            self.stats.opus_ok += 1;
        }
        self.carry.len() >= n_samples
    }

    fn take_play(&mut self) -> Vec<i16> {
        let all = std::mem::take(&mut self.carry);
        let take = all.len().min(FRAME_SAMPLES);
        let mut play = all[..take].to_vec();
        play.resize(FRAME_SAMPLES, 0);
        self.carry = if all.len() > FRAME_SAMPLES {
            all[FRAME_SAMPLES..].to_vec()
        } else {
            Vec::new()
        };
        play
    }

    fn emit(&mut self, decoded: Vec<i16>) -> Vec<i16> {
        self.carry.extend_from_slice(&decoded);
        self.take_play()
    }

    fn decode(&mut self, payload: Option<&[u8]>, fec: bool) -> Vec<i16> {
        // FEC and PLC decode exactly one 20 ms frame (like the JS "size = FRAME");
        // a normal packet may decode more and uses the full buffer.
        let conceal = fec || payload.is_none_or(|p| p.is_empty());
        let max = if conceal { FRAME_SAMPLES } else { 1920 };
        let mut pcm = vec![0_i16; max];
        let result = if fec {
            self.decoder
                .decode_with_fec(payload.unwrap_or(&[]), true, &mut pcm)
        } else if conceal {
            self.decoder.decode_loss(&mut pcm)
        } else {
            self.decoder.decode(payload.unwrap(), &mut pcm)
        };
        match result {
            Ok(n) if n > 0 => {
                pcm.truncate(n.min(max));
                pcm
            }
            _ => {
                self.stats.opus_fail += 1;
                vec![0_i16; FRAME_SAMPLES]
            }
        }
    }

    fn discard_to(&mut self, keep: usize) -> u32 {
        if self.packets.is_empty() || self.next.is_none() {
            return 0;
        }
        let last = *self.packets.keys().next_back().unwrap();
        let extra = i64::from(last) - i64::from(self.next.unwrap()) + 1 - keep as i64;
        if extra <= 0 {
            return 0;
        }
        let cut = self.next.unwrap() + extra as u32;
        let drop: Vec<u32> = self
            .packets
            .keys()
            .filter(|&&idx| idx < cut)
            .copied()
            .collect();
        for idx in drop {
            self.packets.remove(&idx);
        }
        self.next = self.packets.keys().next().copied().or(Some(cut));
        self.carry.clear();
        self.expand = 0;
        self.stats.reset += 1;
        extra as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zcall_core::audio::OpusEncoder;

    fn tone() -> Vec<i16> {
        (0..FRAME_SAMPLES)
            .map(|i| {
                let t = i as f64 / 16_000.0;
                (8_000.0 * (2.0 * std::f64::consts::PI * 440.0 * t).sin()) as i16
            })
            .collect()
    }

    #[test]
    fn plays_packets_in_order() {
        let mut encoder = OpusEncoder::new().expect("encoder");
        let payload = encoder.encode_frame(&tone()).expect("encode");
        let mut jb = JitterBuffer::new(10, Some(5)).expect("jitter");
        for idx in 0..5 {
            jb.push(idx, &payload, 0.0);
        }
        for _ in 0..5 {
            let frame = jb.pull().expect("frame");
            assert_eq!(frame.len(), FRAME_SAMPLES);
        }
        assert!(jb.stats.opus_ok >= 5);
    }

    #[test]
    fn counts_late_and_duplicate_packets() {
        let mut encoder = OpusEncoder::new().expect("encoder");
        let payload = encoder.encode_frame(&tone()).expect("encode");
        let mut jb = JitterBuffer::new(10, Some(5)).expect("jitter");
        for idx in 5..10 {
            jb.push(idx, &payload, 0.0);
        }
        assert!(jb.pull().is_some()); // starts at idx 5
        jb.push(4, &payload, 0.0); // late (< next)
        jb.push(6, &payload, 0.0); // duplicate
        assert_eq!(jb.stats.late, 1);
        assert_eq!(jb.stats.dup, 1);
    }

    #[test]
    fn conceals_loss_then_goes_quiet() {
        let mut encoder = OpusEncoder::new().expect("encoder");
        let payload = encoder.encode_frame(&tone()).expect("encode");
        let mut jb = JitterBuffer::new(10, Some(5)).expect("jitter");
        for idx in 0..5 {
            jb.push(idx, &payload, 0.0);
        }
        for _ in 0..5 {
            assert!(jb.pull().is_some());
        }
        // No more packets: PLC for MAX_EXPAND frames, then quiet.
        for _ in 0..MAX_EXPAND {
            assert!(jb.pull().is_some());
        }
        assert!(jb.pull().is_none());
        assert!(jb.stats.plc >= MAX_EXPAND as u64);
    }

    #[test]
    fn resyncs_on_large_sequence_jump_and_shrinks_speech_smoothly() {
        let mut encoder = OpusEncoder::new().expect("encoder");
        let payload = encoder.encode_frame(&tone()).expect("encode");
        let mut jb = JitterBuffer::new(10, Some(3)).expect("jitter");
        for idx in 10..15 {
            jb.push(idx, &payload, 0.0);
        }
        assert!(jb.pull().is_some());
        // Simulate a stream restart / reconnect jump (> RESYNC_GAP behind/ahead).
        for idx in 500..505 {
            jb.push(idx, &payload, 0.1);
        }
        assert!(jb.stats.reset >= 1);
        assert!(jb.pull().is_some());

        // Push a moderate burst past target + 5 so speech WSOLA shrink engages
        // instead of dropping frames abruptly.
        for idx in 505..516 {
            jb.push(idx, &payload, 0.12);
        }
        let resets_before = jb.stats.reset;
        for _ in 0..4 {
            assert!(jb.pull().is_some());
        }
        assert!(
            jb.stats.accelerate >= 1,
            "expected WSOLA speech acceleration on backlog"
        );
        assert_eq!(jb.stats.reset, resets_before);
    }
}
