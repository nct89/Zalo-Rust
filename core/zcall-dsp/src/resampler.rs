//! Phase-continuous windowed-sinc FIR resampler for call audio.
//!
//! Replaces zero-order hold playback and un-filtered point-sampling decimation
//! so hardware rates (48 kHz, 44.1 kHz, 32 kHz, 24 kHz) convert cleanly to and
//! from 16 kHz Opus without aliasing hiss or imaging distortion.

use std::f64::consts::PI;

/// Exact 3x / ÷3 FIR half-order (49 taps at 48 kHz => ~0.5 ms causal delay).
const FIR3_HALF: usize = 24;
const FIR3_TAPS: usize = FIR3_HALF * 2 + 1;
/// 16 kHz lookback needed for 3x polyphase upsampling (`ceil(49 / 3) = 17`, keep 16 prior).
const UP3_HIST: usize = 16;
/// 48 kHz lookback needed for ÷3 anti-aliasing decimation (`FIR3_TAPS - 1`).
const DOWN3_HIST: usize = FIR3_TAPS - 1;

/// Polyphase table parameters for arbitrary fractional sample-rate ratios
/// (e.g. 44.1 kHz <-> 16 kHz).
const GEN_PHASES: usize = 64;
const GEN_TAPS: usize = 24;

/// Stateful, phase-continuous mono FIR resampler.
pub struct PolyphaseResampler {
    in_rate: u32,
    out_rate: u32,
    mode: Mode,
}

enum Mode {
    Identity,
    Up3 {
        fir: [f32; FIR3_TAPS],
        hist: [f32; UP3_HIST],
    },
    Down3 {
        fir: [f32; FIR3_TAPS],
        hist: [f32; DOWN3_HIST],
    },
    General {
        /// `GEN_PHASES + 1` rows of `GEN_TAPS` coefficients for linear phase interpolation.
        bank: Vec<[f32; GEN_TAPS]>,
        /// History of the previous `GEN_TAPS` mono input samples.
        hist: Vec<f32>,
        /// Current fractional read position relative to the start of the new input slice.
        pos: f64,
        /// Input samples consumed per output sample (`in_rate / out_rate`).
        step: f64,
    },
}

impl PolyphaseResampler {
    /// Builds a resampler converting mono audio from `in_rate` Hz to `out_rate` Hz.
    #[must_use]
    pub fn new(in_rate: u32, out_rate: u32) -> Self {
        let in_rate = in_rate.max(1);
        let out_rate = out_rate.max(1);
        let mode = if in_rate == out_rate {
            Mode::Identity
        } else if out_rate == in_rate.saturating_mul(3) {
            Mode::Up3 {
                fir: build_blackman_harris_fir(FIR3_TAPS, 0.148),
                hist: [0.0; UP3_HIST],
            }
        } else if in_rate == out_rate.saturating_mul(3) {
            Mode::Down3 {
                fir: build_blackman_harris_fir(FIR3_TAPS, 0.148),
                hist: [0.0; DOWN3_HIST],
            }
        } else {
            let ratio = f64::from(out_rate) / f64::from(in_rate);
            // Normalized cutoff relative to input sample rate. Keep 10% transition
            // margin below the lower Nyquist so stopband attenuation stays > 60 dB.
            let fc = 0.44 * ratio.min(1.0);
            let bank = build_polyphase_bank(fc);
            Mode::General {
                bank,
                hist: vec![0.0; GEN_TAPS],
                pos: 0.0,
                step: f64::from(in_rate) / f64::from(out_rate),
            }
        };
        Self {
            in_rate,
            out_rate,
            mode,
        }
    }

    /// Configured input sample rate in Hz.
    #[must_use]
    pub fn in_rate(&self) -> u32 {
        self.in_rate
    }

    /// Configured output sample rate in Hz.
    #[must_use]
    pub fn out_rate(&self) -> u32 {
        self.out_rate
    }

    /// Resets internal filter history to silence.
    pub fn reset(&mut self) {
        match &mut self.mode {
            Mode::Identity => {}
            Mode::Up3 { hist, .. } => hist.fill(0.0),
            Mode::Down3 { hist, .. } => hist.fill(0.0),
            Mode::General { hist, pos, .. } => {
                hist.fill(0.0);
                *pos = 0.0;
            }
        }
    }

    /// Resamples one chunk of mono `f32` audio (arbitrary scale, e.g. `-1.0..1.0`
    /// or `-32768.0..32767.0`), preserving exact phase continuity across chunks.
    pub fn process_f32(&mut self, input: &[f32]) -> Vec<f32> {
        if input.is_empty() {
            return Vec::new();
        }
        match &mut self.mode {
            Mode::Identity => input.to_vec(),
            Mode::Up3 { fir, hist } => {
                let mut work = Vec::with_capacity(UP3_HIST + input.len());
                work.extend_from_slice(hist);
                work.extend_from_slice(input);
                hist.copy_from_slice(&work[work.len() - UP3_HIST..]);

                let mut out = Vec::with_capacity(input.len() * 3);
                for i in 0..input.len() {
                    let base = UP3_HIST + i;
                    for phase in 0..3 {
                        let n = 3 * base + phase;
                        let mut acc = 0.0_f32;
                        let mut k = phase;
                        while k < FIR3_TAPS {
                            let src_idx = (n - k) / 3;
                            acc += work[src_idx] * fir[k];
                            k += 3;
                        }
                        out.push(acc * 3.0);
                    }
                }
                out
            }
            Mode::Down3 { fir, hist } => {
                let mut work = Vec::with_capacity(DOWN3_HIST + input.len());
                work.extend_from_slice(hist);
                work.extend_from_slice(input);
                let out_len = input.len() / 3;
                let consumed = out_len * 3;
                if consumed > 0 {
                    hist.copy_from_slice(&work[consumed..consumed + DOWN3_HIST]);
                }
                let mut out = Vec::with_capacity(out_len);
                for i in 0..out_len {
                    let start = i * 3;
                    let mut acc = 0.0_f32;
                    for k in 0..FIR3_TAPS {
                        acc += work[start + (FIR3_TAPS - 1 - k)] * fir[k];
                    }
                    out.push(acc);
                }
                out
            }
            Mode::General {
                bank,
                hist,
                pos,
                step,
            } => {
                let mut work = Vec::with_capacity(GEN_TAPS + input.len());
                work.extend_from_slice(hist);
                work.extend_from_slice(input);
                let in_len = input.len() as f64;
                let mut out = Vec::with_capacity(((in_len / *step).ceil() as usize) + 2);
                while *pos < in_len {
                    let idx_f = *pos;
                    let base = idx_f.floor() as usize;
                    let frac = idx_f - base as f64;
                    let phase_f = frac * GEN_PHASES as f64;
                    let p0 = (phase_f.floor() as usize).min(GEN_PHASES - 1);
                    let p1 = p0 + 1;
                    let alpha = (phase_f - p0 as f64) as f32;
                    let t0 = &bank[p0];
                    let t1 = &bank[p1];
                    let mut acc = 0.0_f32;
                    for k in 0..GEN_TAPS {
                        let w = t0[k] + (t1[k] - t0[k]) * alpha;
                        acc += work[base + k] * w;
                    }
                    out.push(acc);
                    *pos += *step;
                }
                *pos -= in_len;
                hist.copy_from_slice(&work[work.len() - GEN_TAPS..]);
                out
            }
        }
    }

    /// Resamples one chunk of mono `i16` PCM.
    pub fn process_i16(&mut self, input: &[i16]) -> Vec<i16> {
        if input.is_empty() {
            return Vec::new();
        }
        if matches!(self.mode, Mode::Identity) {
            return input.to_vec();
        }
        let in_f: Vec<f32> = input.iter().map(|&s| f32::from(s)).collect();
        let out_f = self.process_f32(&in_f);
        out_f
            .into_iter()
            .map(|s| s.round().clamp(-32_768.0, 32_767.0) as i16)
            .collect()
    }

    /// Downmixes interleaved `f32` (`-1.0..1.0`) capture frames to mono and
    /// resamples to `out_rate` as `i16` PCM.
    pub fn downmix_f32_to_i16(&mut self, interleaved: &[f32], in_channels: u16) -> Vec<i16> {
        let ch = usize::from(in_channels.max(1));
        let frames = interleaved.len() / ch;
        if frames == 0 {
            return Vec::new();
        }
        let inv_ch = 1.0 / ch as f32;
        let mut mono = Vec::with_capacity(frames);
        if ch == 1 {
            mono.extend_from_slice(&interleaved[..frames]);
        } else {
            for frame in interleaved[..frames * ch].chunks_exact(ch) {
                let sum: f32 = frame.iter().copied().sum();
                mono.push(sum * inv_ch);
            }
        }
        let resampled = self.process_f32(&mono);
        resampled
            .into_iter()
            .map(|s| (s.clamp(-1.0, 1.0) * 32_767.0).round().clamp(-32_768.0, 32_767.0) as i16)
            .collect()
    }

    /// Resamples interleaved `i16` playback frames (`in_channels`) to `out_rate`
    /// and expands to `out_channels` interleaved `f32` (`-1.0..1.0`).
    pub fn upmix_i16_to_f32(
        &mut self,
        pcm: &[i16],
        in_channels: u8,
        out_channels: u16,
    ) -> Vec<f32> {
        let in_ch = usize::from(in_channels.max(1));
        let out_ch = usize::from(out_channels.max(1));
        let in_frames = pcm.len() / in_ch;
        if in_frames == 0 {
            return Vec::new();
        }
        let mut mono = Vec::with_capacity(in_frames);
        if in_ch == 1 {
            for &s in &pcm[..in_frames] {
                mono.push(f32::from(s) / 32_768.0);
            }
        } else {
            let inv = 1.0 / (in_ch as f32 * 32_768.0);
            for frame in pcm[..in_frames * in_ch].chunks_exact(in_ch) {
                let sum: f32 = frame.iter().map(|&s| f32::from(s)).sum();
                mono.push(sum * inv);
            }
        }
        let resampled = self.process_f32(&mono);
        if out_ch == 1 {
            return resampled
                .into_iter()
                .map(|s| s.clamp(-1.0, 1.0))
                .collect();
        }
        let mut out = Vec::with_capacity(resampled.len() * out_ch);
        for s in resampled {
            let v = s.clamp(-1.0, 1.0);
            for _ in 0..out_ch {
                out.push(v);
            }
        }
        out
    }
}

/// 4-term Blackman-Harris windowed-sinc lowpass FIR normalized to unity DC gain.
fn build_blackman_harris_fir<const N: usize>(taps: usize, fc: f64) -> [f32; N] {
    debug_assert_eq!(N, taps);
    let half = (taps - 1) as f64 / 2.0;
    let n_max = (taps - 1) as f64;
    let mut raw = [0.0_f64; N];
    let mut sum = 0.0_f64;
    for (idx, slot) in raw.iter_mut().enumerate() {
        let m = idx as f64 - half;
        let sinc = if m.abs() < 1e-12 {
            2.0 * fc
        } else {
            (2.0 * PI * fc * m).sin() / (PI * m)
        };
        let a = 2.0 * PI * idx as f64 / n_max;
        let w = 0.35875 - 0.48829 * a.cos() + 0.14128 * (2.0 * a).cos() - 0.01168 * (3.0 * a).cos();
        let v = sinc * w;
        *slot = v;
        sum += v;
    }
    let mut out = [0.0_f32; N];
    for (dst, &v) in out.iter_mut().zip(raw.iter()) {
        *dst = (v / sum) as f32;
    }
    out
}

/// Builds `GEN_PHASES + 1` polyphase rows of `GEN_TAPS` Blackman-Harris sinc weights.
fn build_polyphase_bank(fc: f64) -> Vec<[f32; GEN_TAPS]> {
    let half = (GEN_TAPS as f64 - 1.0) / 2.0;
    let mut bank = Vec::with_capacity(GEN_PHASES + 1);
    for p in 0..=GEN_PHASES {
        let frac = p as f64 / GEN_PHASES as f64;
        let mut raw = [0.0_f64; GEN_TAPS];
        let mut sum = 0.0_f64;
        for (k, slot) in raw.iter_mut().enumerate() {
            // Sample position relative to the filter center shifted by `frac`.
            let m = (k as f64) - half - frac;
            let sinc = if m.abs() < 1e-12 {
                2.0 * fc
            } else {
                (2.0 * PI * fc * m).sin() / (PI * m)
            };
            // Smooth Blackman-Harris window over [-half-1 .. +half+1].
            let u = ((k as f64 - frac) / (GEN_TAPS as f64 - 1.0)).clamp(0.0, 1.0);
            let a = 2.0 * PI * u;
            let w = 0.35875 - 0.48829 * a.cos() + 0.14128 * (2.0 * a).cos()
                - 0.01168 * (3.0 * a).cos();
            let v = sinc * w;
            *slot = v;
            sum += v;
        }
        let mut row = [0.0_f32; GEN_TAPS];
        let norm = if sum.abs() > 1e-9 { sum } else { 1.0 };
        for (dst, &v) in row.iter_mut().zip(raw.iter()) {
            *dst = (v / norm) as f32;
        }
        bank.push(row);
    }
    bank
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rms_i16(samples: &[i16]) -> f64 {
        if samples.is_empty() {
            return 0.0;
        }
        let s: f64 = samples.iter().map(|&v| f64::from(v) * f64::from(v)).sum();
        (s / samples.len() as f64).sqrt()
    }

    #[test]
    fn round_trips_16k_48k_16k_cleanly_across_frame_boundaries() {
        let mut up = PolyphaseResampler::new(16_000, 48_000);
        let mut down = PolyphaseResampler::new(48_000, 16_000);
        let mut in_all = Vec::new();
        let mut out_all = Vec::new();
        for f in 0..20_usize {
            let frame: Vec<i16> = (0..320)
                .map(|i| {
                    let t = (f * 320 + i) as f64 / 16_000.0;
                    let sig = 0.6 * (2.0 * PI * 1_000.0 * t).sin()
                        + 0.3 * (2.0 * PI * 3_500.0 * t).sin();
                    (sig * 12_000.0).round() as i16
                })
                .collect();
            in_all.extend_from_slice(&frame);
            let up48 = up.process_i16(&frame);
            assert_eq!(up48.len(), 960);
            let back16 = down.process_i16(&up48);
            assert_eq!(back16.len(), 320);
            out_all.extend_from_slice(&back16);
        }
        // Group delay of two 49-tap FIRs at 48 kHz is 24 + 24 = 48 samples @ 48 kHz = 16 samples @ 16 kHz.
        let delay = 16_usize;
        let start = 640;
        let end = in_all.len() - delay;
        let mut sig_pow = 0.0;
        let mut err_pow = 0.0;
        for i in start..end {
            let s = f64::from(in_all[i]);
            let e = f64::from(out_all[i + delay]) - s;
            sig_pow += s * s;
            err_pow += e * e;
        }
        let snr_db = 10.0 * (sig_pow / err_pow.max(1.0)).log10();
        assert!(snr_db > 50.0, "expected round-trip SNR > 50 dB, got {snr_db:.1} dB");
    }

    #[test]
    fn rejects_15khz_aliasing_when_decimating_48k_to_16k() {
        let mut down = PolyphaseResampler::new(48_000, 16_000);
        let mut out_all = Vec::new();
        for f in 0..10_usize {
            let frame: Vec<f32> = (0..960)
                .map(|i| {
                    let t = (f * 960 + i) as f64 / 48_000.0;
                    (0.5 * (2.0 * PI * 15_000.0 * t).sin()) as f32
                })
                .collect();
            out_all.extend_from_slice(&down.downmix_f32_to_i16(&frame, 1));
        }
        // Measure steady-state output after filter warm-up: 15 kHz must be attenuated > 55 dB.
        let in_rms = 0.5 * 32_767.0 / std::f64::consts::SQRT_2;
        let out_rms = rms_i16(&out_all[320..]);
        let atten_db = 20.0 * (in_rms / out_rms.max(0.01)).log10();
        assert!(
            atten_db > 55.0,
            "expected > 55 dB anti-aliasing attenuation at 15 kHz, got {atten_db:.1} dB (out_rms={out_rms:.2})"
        );
    }

    #[test]
    fn fractional_44100_to_16000_rejects_high_frequency_aliasing() {
        let mut down = PolyphaseResampler::new(44_100, 16_000);
        let mut out_all = Vec::new();
        for f in 0..10_usize {
            let frame: Vec<f32> = (0..882)
                .map(|i| {
                    let t = (f * 882 + i) as f64 / 44_100.0;
                    (0.5 * (2.0 * PI * 14_000.0 * t).sin()) as f32
                })
                .collect();
            out_all.extend_from_slice(&down.downmix_f32_to_i16(&frame, 1));
        }
        let in_rms = 0.5 * 32_767.0 / std::f64::consts::SQRT_2;
        let out_rms = rms_i16(&out_all[320..]);
        let atten_db = 20.0 * (in_rms / out_rms.max(0.01)).log10();
        assert!(
            atten_db > 45.0,
            "expected > 45 dB anti-aliasing at 14 kHz (44.1->16k), got {atten_db:.1} dB"
        );
    }
}
