//! WSOLA time-stretch and RMS helpers, ported from the reference call
//! implementation (`audio-io.py`).
//!
//! `wsola_shrink` shortens the playout buffer by one pitch period when it grows
//! past its target, like NetEq's Accelerate. Pure DSP, no Opus or threads.

/// Samples in one 20 ms frame at 16 kHz.
pub const FRAME_SAMPLES: usize = 320;

/// RMS of an i16 frame.
pub fn rms(samples: &[i16]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f64 = samples.iter().map(|v| f64::from(*v) * f64::from(*v)).sum();
    (sum / samples.len() as f64).sqrt()
}

/// Lag, in samples, of one pitch period inside 2.5..10 ms.
pub fn pitch_lag(samples: &[i16]) -> usize {
    let window = 80_usize;
    let limit = 160_usize.min(samples.len().saturating_sub(window + 1));
    if limit < 40 {
        return 80;
    }
    let mut best: Option<i64> = None;
    let mut best_lag = 80_usize;
    for lag in (40..=limit).step_by(2) {
        let mut score = 0_i64;
        for i in 0..window {
            score += i64::from(samples[i]) * i64::from(samples[i + lag]);
        }
        if best.is_none() || score > best.unwrap() {
            best = Some(score);
            best_lag = lag;
        }
    }
    best_lag
}

/// Shortens `pcm` by one pitch period.
///
/// Returns `(20 ms to play, the rest)`. The join is an overlap-add so the
/// sample stream does not jump.
pub fn wsola_shrink(pcm: &[i16]) -> (Vec<i16>, Vec<i16>) {
    if pcm.len() < FRAME_SAMPLES * 2 {
        let mut play = pcm.to_vec();
        play.resize(FRAME_SAMPLES, 0);
        let rest = if pcm.len() > FRAME_SAMPLES {
            pcm[FRAME_SAMPLES..].to_vec()
        } else {
            Vec::new()
        };
        return (play, rest);
    }
    let lag = pitch_lag(pcm);
    let mut out = vec![0_i16; pcm.len() - lag];
    for i in 0..lag {
        let weight = (i as f64 + 1.0) / (lag as f64 + 1.0);
        let mixed = f64::from(pcm[i]) * (1.0 - weight) + f64::from(pcm[i + lag]) * weight;
        out[i] = clamp_i16(mixed.round());
    }
    let rest_len = pcm.len() - 2 * lag;
    for i in 0..rest_len {
        out[lag + i] = pcm[2 * lag + i];
    }
    let play = out[..FRAME_SAMPLES].to_vec();
    let rest = out[FRAME_SAMPLES..].to_vec();
    (play, rest)
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

    fn sine_1k(amplitude: i16, samples: usize) -> Vec<i16> {
        (0..samples)
            .map(|i| {
                let t = i as f64 / 16_000.0;
                (f64::from(amplitude) * (2.0 * std::f64::consts::PI * 1000.0 * t).sin()) as i16
            })
            .collect()
    }

    #[test]
    fn rms_matches_full_scale() {
        assert!(rms(&[32_000_i16; 320]) > 30_000.0);
        assert_eq!(rms(&[]), 0.0);
    }

    #[test]
    fn pitch_lag_of_a_1khz_tone_is_a_harmonic() {
        let lag = pitch_lag(&sine_1k(10_000, 640));
        assert!((40..=160).contains(&lag), "lag {lag}");
        assert_eq!(lag % 16, 0, "1 kHz period 16 -> lag {lag}");
    }

    #[test]
    fn wsola_shrink_returns_one_frame_plus_rest() {
        let samples = sine_1k(10_000, 640);
        let lag = pitch_lag(&samples);
        let (play, rest) = wsola_shrink(&samples);
        assert_eq!(play.len(), FRAME_SAMPLES);
        assert_eq!(play.len() + rest.len(), 640 - lag);
    }

    #[test]
    fn wsola_shrink_short_input_pads_to_one_frame() {
        let (play, rest) = wsola_shrink(&[1_i16, 2, 3]);
        assert_eq!(play.len(), FRAME_SAMPLES);
        assert!(rest.is_empty());
        assert_eq!(&play[..3], &[1, 2, 3]);
    }
}
