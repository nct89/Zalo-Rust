//! The playout mixer (`Playout`), ported from the reference call
//! implementation (`audio-io.py`).
//!
//! Every 20 ms it pulls the next frame of each source and sums them, with the
//! limiter holding a multi-voice mix to about -2 dBFS. The `pacat` write and
//! the 20 ms pacing loop are left to the caller; `mix` and `far_active` are
//! pure and testable.

use crate::capture::Speaker;
use crate::jitter::JitterBuffer;
use crate::wsola::{rms, FRAME_SAMPLES};
use std::collections::BTreeMap;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// A source that sent nothing for this long is forgotten.
pub const IDLE_SEC: f64 = 5.0;

/// Two full-scale voices summed would clip; hold the mix around here.
pub const LIMIT: i32 = 26_000;

/// RMS above which the far end counts as talking.
pub const FAR_RMS: f64 = 300.0;

/// How long the echo may come back after the far end spoke.
pub const FAR_HOLD_SEC: f64 = 0.35;

/// The playout mixer. Source key `None` is the single 1-1 peer.
pub struct Mixer {
    sources: BTreeMap<Option<u32>, JitterBuffer>,
    max_target: usize,
    min_target: Option<usize>,
    limiter: f64,
    far_until: f64,
    /// Mixed frames, for diagnostics.
    pub mixed: u64,
}

impl Mixer {
    /// Builds an empty mixer.
    ///
    /// # Errors
    ///
    /// Returns the Opus error when a decoder cannot be created lazily (it is
    /// created on the first packet of each source).
    pub fn new(max_target: usize, min_target: Option<usize>) -> Self {
        Self {
            sources: BTreeMap::new(),
            max_target,
            min_target,
            limiter: 1.0,
            far_until: 0.0,
            mixed: 0,
        }
    }

    /// Updates the floor and ceiling on the mixer and on sources already open.
    pub fn set_limits(&mut self, max_target: usize, min_target: usize) {
        let min_target = 2.max(min_target);
        let max_target = min_target.max(max_target);
        self.max_target = max_target;
        self.min_target = Some(min_target);
        for jitter in self.sources.values_mut() {
            jitter.set_limits(max_target, min_target);
        }
    }

    /// Routes one decoded-to-Opus packet to a source's jitter buffer.
    pub fn push(&mut self, idx: u32, payload: &[u8], source: Option<u32>, now: f64) {
        let jitter = self.sources.entry(source).or_insert_with(|| {
            JitterBuffer::new(self.max_target, self.min_target).expect("opus decoder")
        });
        jitter.push(idx, payload, now);
    }

    /// Mixes the next 20 ms of every source.
    pub fn mix(&mut self, now: f64) -> Vec<i16> {
        let mut frames = Vec::new();
        let mut voices = 0_usize;
        let mut gone = Vec::new();
        for (source, jitter) in self.sources.iter_mut() {
            if now - jitter.last() > IDLE_SEC && jitter.is_idle() {
                gone.push(*source);
                continue;
            }
            if let Some(pcm) = jitter.pull() {
                frames.push(pcm);
                voices += 1;
            }
        }
        for source in gone {
            self.sources.remove(&source);
        }

        let mut acc: Option<Vec<i32>> = None;
        for frame in frames {
            match acc.as_mut() {
                None => acc = Some(frame.iter().map(|v| i32::from(*v)).collect()),
                Some(acc) => {
                    for (i, sample) in frame.iter().enumerate() {
                        acc[i] += i32::from(*sample);
                    }
                }
            }
        }
        self.mixed += 1;
        match acc {
            None => vec![0_i16; FRAME_SAMPLES],
            Some(acc) => self.limit(acc, voices),
        }
    }

    /// Records the played frame for echo detection.
    pub fn note_played(&mut self, pcm: &[i16], now: f64) {
        if rms(pcm) >= FAR_RMS {
            self.far_until = now + FAR_HOLD_SEC;
        }
    }

    /// Whether the far end is talking (to hold the MicLeveler gain).
    pub fn far_active(&self, now: f64) -> bool {
        now < self.far_until
    }

    /// Number of live sources.
    pub fn source_count(&self) -> usize {
        self.sources.len()
    }

    fn limit(&mut self, acc: Vec<i32>, voices: usize) -> Vec<i16> {
        let prev = self.limiter;
        if voices < 2 {
            self.limiter += (1.0 - self.limiter) * 0.1;
            let next = self.limiter;
            if (prev - 1.0).abs() < 1e-4 && (next - 1.0).abs() < 1e-4 {
                return acc.iter().map(|v| clamp(*v)).collect();
            }
            let n = acc.len().max(1) as f64;
            let step = (next - prev) / n;
            return acc
                .iter()
                .enumerate()
                .map(|(i, v)| clamp((f64::from(*v) * (prev + step * (i as f64 + 1.0))) as i32))
                .collect();
        }
        // `i32::abs` panics on `i32::MIN` in a debug build.
        let peak = acc.iter().map(|v| v.unsigned_abs()).max().unwrap_or(0);
        let target = if peak <= LIMIT as u32 {
            1.0
        } else {
            f64::from(LIMIT) / f64::from(peak)
        };
        if target < self.limiter {
            self.limiter = target;
        } else {
            self.limiter += (target - self.limiter) * 0.1;
        }
        let next = self.limiter;
        // Attack immediately when gain drops so multi-speaker peaks never clip;
        // ramp smoothly across the 20 ms frame on release to prevent clicks.
        if next < prev {
            return acc
                .iter()
                .map(|v| clamp((f64::from(*v) * next) as i32))
                .collect();
        }
        let n = acc.len().max(1) as f64;
        let step = (next - prev) / n;
        acc.iter()
            .enumerate()
            .map(|(i, v)| clamp((f64::from(*v) * (prev + step * (i as f64 + 1.0))) as i32))
            .collect()
    }
}

fn clamp(value: i32) -> i16 {
    if value > 32767 {
        32767
    } else if value < -32768 {
        -32768
    } else {
        value as i16
    }
}

/// A PCM sink that receives one 320-sample frame per tick.
pub trait PcmSink {
    /// Writes one frame of i16 PCM.
    fn write_pcm(&mut self, pcm: &[i16]) -> io::Result<()>;
}

impl PcmSink for Speaker {
    fn write_pcm(&mut self, pcm: &[i16]) -> io::Result<()> {
        self.write(pcm)
    }
}

/// One 20 ms tick: mix, note the far-end level, write to the sink.
pub fn playout_step(mixer: &mut Mixer, sink: &mut impl PcmSink, now: f64) -> io::Result<()> {
    let pcm = mixer.mix(now);
    mixer.note_played(&pcm, now);
    sink.write_pcm(&pcm)
}

/// Runs the playout loop, paced 20 ms by our own clock (the non-PulseOut path).
pub fn run_playout(mixer: &mut Mixer, sink: &mut impl PcmSink, stop: &AtomicBool) {
    let mut next = Instant::now();
    while !stop.load(Ordering::SeqCst) {
        let now = monotonic_secs();
        if playout_step(mixer, sink, now).is_err() {
            break;
        }
        next += Duration::from_millis(20);
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        } else if now - next > Duration::from_millis(200) {
            next = now;
        }
    }
}

fn monotonic_secs() -> f64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64()
}

#[cfg(test)]
mod tests {
    use super::*;
    use zcall_core::audio::OpusEncoder;

    fn tone(amplitude: i16) -> Vec<i16> {
        (0..FRAME_SAMPLES)
            .map(|i| {
                let t = i as f64 / 16_000.0;
                (f64::from(amplitude) * (2.0 * std::f64::consts::PI * 440.0 * t).sin()) as i16
            })
            .collect()
    }

    fn encoded_tone(amplitude: i16) -> Vec<u8> {
        let mut encoder = OpusEncoder::new().expect("encoder");
        encoder.encode_frame(&tone(amplitude)).expect("encode")
    }

    #[test]
    fn mixes_silence_with_no_sources() {
        let mut mixer = Mixer::new(10, Some(5));
        let out = mixer.mix(0.0);
        assert_eq!(out.len(), FRAME_SAMPLES);
        assert!(out.iter().all(|v| *v == 0));
    }

    #[test]
    fn mixes_two_sources() {
        let mut mixer = Mixer::new(10, Some(5));
        let payload = encoded_tone(8000);
        for idx in 0..5 {
            mixer.push(idx, &payload, Some(1), 0.0);
            mixer.push(idx, &payload, Some(2), 0.0);
        }
        let out = mixer.mix(0.0);
        assert_eq!(out.len(), FRAME_SAMPLES);
        assert_eq!(mixer.source_count(), 2);
        let peak = out.iter().map(|v| v.saturating_abs()).max().unwrap_or(0);
        assert!(peak > 0, "mixed output should not be silent");
    }

    #[test]
    fn detects_far_speech() {
        let mut mixer = Mixer::new(10, Some(5));
        let loud = tone(20_000);
        mixer.note_played(&loud, 0.0);
        assert!(mixer.far_active(0.1));
        assert!(!mixer.far_active(1.0));
    }

    #[test]
    fn forgets_an_idle_source() {
        let mut mixer = Mixer::new(10, Some(5));
        let payload = encoded_tone(1000);
        for idx in 0..5 {
            mixer.push(idx, &payload, Some(7), 0.0);
        }
        assert_eq!(mixer.source_count(), 1);
        for _ in 0..5 {
            let _ = mixer.mix(0.0); // drain the packets
        }
        // No packets, no carry, and idle past IDLE_SEC: the source is gone.
        mixer.mix(IDLE_SEC + 1.0);
        assert_eq!(mixer.source_count(), 0);
    }

    struct Recorder {
        frames: Vec<Vec<i16>>,
    }

    impl PcmSink for Recorder {
        fn write_pcm(&mut self, pcm: &[i16]) -> io::Result<()> {
            self.frames.push(pcm.to_vec());
            Ok(())
        }
    }

    #[test]
    fn playout_step_writes_one_frame() {
        let mut mixer = Mixer::new(10, Some(5));
        let payload = encoded_tone(8000);
        for idx in 0..5 {
            mixer.push(idx, &payload, None, 0.0);
        }
        let mut sink = Recorder { frames: Vec::new() };
        playout_step(&mut mixer, &mut sink, 0.0).expect("step");
        assert_eq!(sink.frames.len(), 1);
        assert_eq!(sink.frames[0].len(), FRAME_SAMPLES);
    }

    #[test]
    fn smooth_limiter_prevents_multi_speaker_clipping() {
        let mut mixer = Mixer::new(10, Some(3));
        let loud = encoded_tone(26_000);
        for idx in 0..6 {
            mixer.push(idx, &loud, Some(10), 0.0);
            mixer.push(idx, &loud, Some(20), 0.0);
        }
        let first = mixer.mix(0.0);
        let second = mixer.mix(0.02);
        let peak1 = first.iter().map(|v| v.saturating_abs()).max().unwrap_or(0);
        let peak2 = second.iter().map(|v| v.saturating_abs()).max().unwrap_or(0);
        assert!(peak1 > 10_000 && peak1 < i16::MAX);
        assert!(peak2 > 10_000 && peak2 < i16::MAX);
        assert!(mixer.limiter < 1.0);
    }
}
