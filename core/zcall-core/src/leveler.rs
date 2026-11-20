//! Microphone make-up gain (MicLeveler) and its high-pass filter, ported
//! from the reference call implementation (`audio-io.py`).
//!
//! This is the pure DSP part of the audio pipeline (the sample-level AGC and
//! RBJ high-pass). File persistence, logging and the far-end callback are
//! left to the caller; `process` takes `far_active` per frame instead.

use std::collections::VecDeque;

/// RMS level of an i16 PCM frame in dBFS.
pub fn level_db(samples: &[i16]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f64 = samples.iter().map(|v| f64::from(*v) * f64::from(*v)).sum();
    let rms = (sum / samples.len() as f64).sqrt();
    20.0 * (rms.max(1.0) / 32768.0).log10()
}

/// Second-order Butterworth high-pass (RBJ biquad) at 16 kHz.
pub struct HighPass {
    b0: f64,
    b1: f64,
    a1: f64,
    a2: f64,
    x1: f64,
    x2: f64,
    y1: f64,
    y2: f64,
}

impl HighPass {
    /// Builds a high-pass at `hz` for a `rate` Hz stream.
    pub fn new(hz: f64, rate: f64) -> Self {
        let w = 2.0 * std::f64::consts::PI * hz / rate;
        let alpha = w.sin() / std::f64::consts::SQRT_2;
        let c = w.cos();
        let a0 = 1.0 + alpha;
        Self {
            b0: (1.0 + c) / 2.0 / a0,
            b1: -(1.0 + c) / a0,
            a1: -2.0 * c / a0,
            a2: (1.0 - alpha) / a0,
            x1: 0.0,
            x2: 0.0,
            y1: 0.0,
            y2: 0.0,
        }
    }

    /// Filters one frame, carrying state across frames.
    pub fn process(&mut self, samples: &[i16]) -> Vec<i16> {
        let mut out = Vec::with_capacity(samples.len());
        for &sample in samples {
            let x = f64::from(sample);
            let y =
                self.b0 * (x + self.x2) + self.b1 * self.x1 - self.a1 * self.y1 - self.a2 * self.y2;
            self.x2 = self.x1;
            self.x1 = x;
            self.y2 = self.y1;
            self.y1 = y;
            out.push(clamp_i16(y));
        }
        out
    }
}

/// The soft make-up gain, aiming quiet speech at −22 dBFS.
///
/// Official Zalo uses one fixed-digital AGC (`agcMode` 3, `agcGain` 9 dB,
/// target −3 dBFS) plus one analog mic level. PipeWire's `gain_control`
/// turns on two digital AGCs and was measured to distort loud speech, so
/// this stage stays a single slow gain. The ceiling is the official 9 dB,
/// not +24 dB: that much gain turned the noise floor into a crackle and,
/// once a vowel peaked, the old per-frame limiter flattened the whole word
/// into that crackle.
pub struct MicLeveler {
    /// Current adaptive gain in dB.
    pub gain_db: f64,
    /// Gain applied this frame. It tracks `gain_db`; it does not duck between words.
    pub applied_db: f64,
    /// The learned noise floor in dBFS.
    pub noise_db: f64,
    gain_prev: f64,
    levels: VecDeque<f64>,
    learned: bool,
    n: u64,
    hp: Option<HighPass>,
}

const TARGET_DB: f64 = -22.0;
const NOISE_CEIL_DB: f64 = -38.0;
/// Official `agcGain`. +24 dB clipped vowels into a square wave.
const MAX_GAIN_DB: f64 = 9.0;
/// Peak after gain, official `agcTargetLevel` 3 (−3 dBFS). A 0 dBFS clamp
/// is already the onset of harsh clipping.
const LIMIT_PEAK: f64 = 23_170.0;
const SPEECH_OVER_NOISE_DB: f64 = 9.0;
const HIGH_PASS_HZ: f64 = 100.0;
const SILENCE_DB: f64 = -90.0;
const FLOOR_DB: f64 = -60.0;
/// One second of 20 ms frames before any gain, matching Electron.
const LEARN_FRAMES: usize = 50;
/// Five seconds of frame levels, matching Electron's deque.
const NOISE_FRAMES: usize = 250;
/// How fast the floor may rise, per update. A sentence must not become the floor.
const NOISE_RISE_DB: f64 = 0.25;
/// Largest gain change in one 20 ms frame. Half a dB per frame was an audible step.
const MAX_STEP_DB: f64 = 0.08;

impl MicLeveler {
    /// Builds a leveler; `high_pass` toggles the 100 Hz high-pass.
    pub fn new(high_pass: bool) -> Self {
        Self {
            gain_db: 0.0,
            applied_db: 0.0,
            noise_db: -70.0,
            gain_prev: 1.0,
            levels: VecDeque::new(),
            learned: false,
            n: 0,
            hp: high_pass.then(|| HighPass::new(HIGH_PASS_HZ, 16_000.0)),
        }
    }

    /// Resets the learned state for a new microphone.
    pub fn relearn(&mut self) {
        self.levels.clear();
        self.learned = false;
        self.noise_db = -70.0;
        self.gain_db = 0.0;
        self.applied_db = 0.0;
        self.gain_prev = 1.0;
        if self.hp.is_some() {
            self.hp = Some(HighPass::new(HIGH_PASS_HZ, 16_000.0));
        }
    }

    /// Processes one 320-sample frame.
    pub fn process(&mut self, pcm: &[i16], far_active: bool) -> Vec<i16> {
        let mut samples = pcm.to_vec();
        let mut silent = level_db(&samples) <= SILENCE_DB;
        if let Some(hp) = &mut self.hp {
            samples = hp.process(&samples);
        }
        let level = level_db(&samples);
        silent = silent || level <= SILENCE_DB;

        // Speech must not enter the noise window. Once the floor is known, a
        // frame 9 dB above it is the voice. Pushing those frames made five
        // seconds of continuous speech look like the noise floor, the ceiling
        // fell to 0 dB, and the gain was cut out from under the sentence.
        let speech_like = self.learned && level > self.noise_db + SPEECH_OVER_NOISE_DB;
        if !far_active && !silent && !speech_like {
            push_limited(&mut self.levels, level, NOISE_FRAMES);
        }
        self.n += 1;

        if self.levels.len() >= LEARN_FRAMES && (self.n % 25 == 0 || !self.learned) {
            let mut sorted: Vec<f64> = self.levels.iter().copied().collect();
            sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            // 10th percentile of the pauses. The 2nd percentile sat on digital
            // silence and then allowed +24 dB.
            let floor = sorted[self.levels.len() / 10];
            if !self.learned || floor < self.noise_db {
                self.noise_db = floor;
            } else {
                self.noise_db = floor.min(self.noise_db + NOISE_RISE_DB);
            }
            self.learned = true;
        }

        let voiced =
            self.learned && level > FLOOR_DB && level > self.noise_db + SPEECH_OVER_NOISE_DB;
        // Far-end speech is not a level to chase: raising gain on it pumps the uplink.
        if voiced && !far_active {
            let ceiling = MAX_GAIN_DB.min((NOISE_CEIL_DB - self.noise_db).max(0.0));
            // A ceiling of 0 means the floor is already loud. Hold the gain.
            // Driving it to 0 here is what cut a continuous sentence.
            if ceiling > 0.0 {
                let want = (TARGET_DB - level).clamp(0.0, ceiling);
                self.gain_db += (want - self.gain_db).clamp(-MAX_STEP_DB, MAX_STEP_DB);
            }
        }
        // One gain for speech and pause. Ducking to the noise ceiling between
        // words, then restoring the full gain on the next frame, was a step of
        // several dB at every onset.
        self.applied_db = self.gain_db;

        let gain = 10_f64.powf(self.applied_db / 20.0);
        let gain0 = self.gain_prev;
        // Remember the slow gain, never the peak limit. Writing the limited
        // gain back made the next frame ramp up across 20 ms: the vowel
        // started crushed and the noise at the end of the frame was full gain,
        // so the far end heard crackle with the voice gone.
        self.gain_prev = gain;

        // Always scale, including the first second while makeup gain is still
        // 0 dB. Skipping that left a hot vowel at 0 dBFS, which is already
        // the harsh edge the far end hears as crackle on top of the voice.
        let n = samples.len();
        let step = (gain - gain0) / n as f64;
        let mut shaped = vec![0_f64; n];
        let mut peak = 0_f64;
        for (i, &value) in samples.iter().enumerate() {
            let x = f64::from(value) * (gain0 + step * (i as f64 + 1.0));
            shaped[i] = x;
            peak = peak.max(x.abs());
        }
        // Keep the waveform. Scaling the whole frame down to −3 dBFS holds the
        // shape; clamping each hot sample first turned the vowel into a square
        // wave (the crackle with no voice left in it).
        let scale = if peak > LIMIT_PEAK {
            LIMIT_PEAK / peak
        } else {
            1.0
        };
        shaped.into_iter().map(|x| clamp_i16(x * scale)).collect()
    }
}

fn push_limited(deque: &mut VecDeque<f64>, value: f64, max: usize) {
    if deque.len() == max {
        deque.pop_front();
    }
    deque.push_back(value);
}

fn clamp_i16(value: f64) -> i16 {
    if value > 32767.0 {
        32767
    } else if value < -32768.0 {
        -32768
    } else {
        value as i16
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(amplitude: i16, samples: usize) -> Vec<i16> {
        (0..samples)
            .map(|i| {
                let t = i as f64 / 16_000.0;
                (f64::from(amplitude) * (2.0 * std::f64::consts::PI * 440.0 * t).sin()) as i16
            })
            .collect()
    }

    #[test]
    fn level_db_matches_full_scale() {
        let full = vec![32_000_i16; 320];
        assert!((level_db(&full) - (-0.2)).abs() < 0.5);
        assert!(level_db(&[0_i16; 320]) <= -80.0);
    }

    #[test]
    fn high_pass_removes_dc() {
        let mut hp = HighPass::new(100.0, 16_000.0);
        // Constant offset = DC: the high-pass drives it to zero after a few frames.
        let mut out = Vec::new();
        for _ in 0..10 {
            out = hp.process(&[1000_i16; 320]);
        }
        let peak = out.iter().map(|v| v.saturating_abs()).max().unwrap_or(0);
        assert!(peak < 100, "DC should be attenuated, peak {peak}");
    }

    #[test]
    fn leveler_raises_speech_above_noise() {
        let mut leveler = MicLeveler::new(false);
        // Alternate: 3 speech frames (loud) then 3 pause frames (quiet), so the
        // noise floor is the pause and the speech is clearly voiced.
        for _ in 0..40 {
            for _ in 0..3 {
                // About −35 dBFS: under the −22 dBFS target, so gain must rise.
                leveler.process(&sine(800, 320), false);
            }
            for _ in 0..3 {
                leveler.process(&sine(150, 320), false);
            }
        }
        assert!(leveler.learned);
        assert!(
            leveler.gain_db > 2.0,
            "quiet speech should be raised, gain {:.2} dB",
            leveler.gain_db
        );
    }

    #[test]
    fn continuous_speech_does_not_drop_the_gain() {
        let mut leveler = MicLeveler::new(false);
        for _ in 0..50 {
            leveler.process(&sine(150, 320), false);
        }
        for _ in 0..80 {
            leveler.process(&sine(800, 320), false);
        }
        let gained = leveler.gain_db;
        assert!(gained > 2.0, "expected makeup gain, got {gained}");
        for _ in 0..250 {
            leveler.process(&sine(800, 320), false);
        }
        assert!(
            leveler.gain_db + 1.0 > gained,
            "continuous speech dropped gain from {gained} to {}",
            leveler.gain_db
        );
    }

    #[test]
    fn a_loud_vowel_keeps_its_shape_and_the_next_frame_keeps_the_gain() {
        let mut leveler = MicLeveler::new(false);
        for _ in 0..50 {
            leveler.process(&sine(150, 320), false);
        }
        for _ in 0..200 {
            leveler.process(&sine(800, 320), false);
        }
        let gained = leveler.gain_db;
        assert!(gained > 2.0, "expected makeup, got {gained}");
        let loud = vec![12_000_i16; 320];
        let out = leveler.process(&loud, false);
        let peak = out.iter().map(|v| v.saturating_abs()).max().unwrap_or(0);
        assert!(peak > 8_000, "loud frame was ducked to {peak}");
        assert!(
            (peak as f64) < LIMIT_PEAK + 2.0,
            "loud frame clipped past −3 dBFS, peak {peak}"
        );
        // The slow gain may still step 0.08 dB on this loud frame. That is a
        // ratio of about 1.01. A limiter that ramps from a crushed gain back
        // to full gain across the frame is several times larger.
        let hi = out.iter().map(|v| v.saturating_abs()).max().unwrap_or(0) as f64;
        let lo = out.iter().map(|v| v.saturating_abs()).min().unwrap_or(0) as f64;
        assert!(
            hi / lo.max(1.0) < 1.05,
            "a flat frame was ramped, hi {hi} lo {lo}"
        );
        let next = leveler.process(&sine(800, 320), false);
        let next_peak = next.iter().map(|v| v.saturating_abs()).max().unwrap_or(0);
        assert!(
            next_peak > 1_600,
            "gain collapsed after one peak: next peak {next_peak}, gain {}",
            leveler.gain_db
        );
        assert!(
            (leveler.gain_db - gained).abs() < 1.0,
            "one peak moved the slow gain from {gained} to {}",
            leveler.gain_db
        );
    }

    #[test]
    fn leveler_ignores_digital_silence() {
        let mut leveler = MicLeveler::new(false);
        let silence = vec![0_i16; 320];
        for _ in 0..100 {
            let out = leveler.process(&silence, false);
            assert!(out.iter().all(|v| *v == 0));
        }
        assert_eq!(leveler.gain_db, 0.0);
    }

    #[test]
    fn full_scale_negative_sample_does_not_panic() {
        let mut leveler = MicLeveler::new(true);
        let frame = vec![i16::MIN; 320];
        let out = leveler.process(&frame, false);
        assert_eq!(out.len(), frame.len());
        let again = leveler.process(&out, false);
        assert_eq!(again.len(), frame.len());
    }
}
