//! In-process WebRTC Audio Processing (`sonora`: AEC3, leveled NS, AGC2 +
//! limiter, optional high-pass filter) matching Zalo Windows (`ZaloCall.exe`)
//! and Android (`libzrtc.so`).
//!
//! Operates natively at 16 kHz mono on 10 ms subframes (160 samples × 2 = one
//! 20 ms Opus frame). Playout (`zcall-play`) feeds each played 20 ms frame and
//! its Pulse latency as the AEC render reference; capture (`zcall-mic`) feeds
//! each captured 20 ms frame and its Pulse latency into capture processing.
//! No external `pipewire -c` subprocess or resampling across mismatched quanta
//! is required.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;

use sonora::config::{
    AdaptiveDigital, EchoCanceller, EchoCanceller3Config, FixedDigital, GainController2,
    HighPassFilter, NoiseSuppression, NoiseSuppressionLevel, TransparentModeType,
};
use sonora::{AudioProcessing as SonoraApm, Config, StreamConfig};
use zcall_core::zrtc_helpers::AudioProcessingOptions;

/// Sample rate shared with Opus and PulseAudio streams (Hz).
pub const APM_SAMPLE_RATE: u32 = 16_000;
/// Samples per WebRTC 10 ms subframe at 16 kHz mono.
pub const APM_SUBFRAME_SAMPLES: usize = 160;
/// Default speaker latency estimate (ms) before the first `get_latency` sample.
const DEFAULT_PLAY_LATENCY_MS: u32 = 60;

/// Thread-safe in-process WebRTC APM shared between playout (`zcall-play`) and
/// capture (`zcall-mic`).
pub struct InProcessApm {
    inner: Mutex<SonoraApm>,
    play_latency_ms: AtomicU32,
    enabled: AtomicBool,
    gain_control: AtomicBool,
}

impl InProcessApm {
    /// Builds a 16 kHz mono WebRTC APM configured from `zrtcConfig`.
    pub fn new(opts: AudioProcessingOptions) -> Self {
        let apm = build_sonora_apm(opts);
        let active = opts.aec || opts.noise_suppression || opts.gain_control || opts.high_pass;
        Self {
            inner: Mutex::new(apm),
            play_latency_ms: AtomicU32::new(DEFAULT_PLAY_LATENCY_MS),
            enabled: AtomicBool::new(active),
            gain_control: AtomicBool::new(opts.gain_control),
        }
    }

    /// Returns whether at least one DSP stage is currently enabled.
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }

    /// Returns whether sonora's AGC2 stage is enabled (false by default so
    /// `MicLeveler` stays the single smooth leveler).
    pub fn has_gain_control(&self) -> bool {
        self.gain_control.load(Ordering::Acquire)
    }

    /// Reconfigures AEC / NS / HPF / AGC live in-place without reopening mic
    /// or speaker streams.
    pub fn apply_options(&self, opts: AudioProcessingOptions) {
        let active = opts.aec || opts.noise_suppression || opts.gain_control || opts.high_pass;
        self.enabled.store(active, Ordering::Release);
        self.gain_control.store(opts.gain_control, Ordering::Release);
        let cfg = build_sonora_config(opts);
        let mut apm = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        apm.apply_config(cfg);
    }

    /// Rebuilds the internal DSP state cleanly (for new calls or hardware
    /// mic/speaker hot-swaps where the acoustic echo path changes).
    pub fn reset(&self, opts: AudioProcessingOptions) {
        let active = opts.aec || opts.noise_suppression || opts.gain_control || opts.high_pass;
        self.enabled.store(active, Ordering::Release);
        self.gain_control.store(opts.gain_control, Ordering::Release);
        self.play_latency_ms
            .store(DEFAULT_PLAY_LATENCY_MS, Ordering::Relaxed);
        let fresh = build_sonora_apm(opts);
        let mut apm = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        *apm = fresh;
    }

    /// Feeds one played (far-end) PCM buffer (typically 320 samples = 20 ms)
    /// into AEC3's reverse stream along with the speaker's measured output
    /// latency in milliseconds.
    pub fn feed_render(&self, pcm: &[i16], play_latency_ms: u32) {
        self.play_latency_ms
            .store(play_latency_ms.min(400), Ordering::Relaxed);
        if !self.enabled.load(Ordering::Acquire) || pcm.len() < APM_SUBFRAME_SAMPLES {
            return;
        }
        let mut apm = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut scratch = [0_i16; APM_SUBFRAME_SAMPLES];
        for chunk in pcm.chunks_exact(APM_SUBFRAME_SAMPLES) {
            let _ = apm.process_render_i16(chunk, &mut scratch);
        }
    }

    /// Processes one captured (near-end) PCM buffer (typically 320 samples =
    /// 20 ms) through AEC3 → NS → HPF → AGC2/Limiter using the combined
    /// render + capture latency as WebRTC's stream delay.
    pub fn process_capture(&self, pcm: &[i16], cap_latency_ms: u32) -> Vec<i16> {
        if !self.enabled.load(Ordering::Acquire) || pcm.len() < APM_SUBFRAME_SAMPLES {
            return pcm.to_vec();
        }
        let play_ms = self.play_latency_ms.load(Ordering::Relaxed);
        let total_delay_ms = play_ms.saturating_add(cap_latency_ms).min(500) as i32;
        let mut apm = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let _ = apm.set_stream_delay_ms(total_delay_ms);

        let mut out = vec![0_i16; pcm.len()];
        let full = (pcm.len() / APM_SUBFRAME_SAMPLES) * APM_SUBFRAME_SAMPLES;
        for (src, dst) in pcm[..full]
            .chunks_exact(APM_SUBFRAME_SAMPLES)
            .zip(out[..full].chunks_exact_mut(APM_SUBFRAME_SAMPLES))
        {
            if apm.process_capture_i16(src, dst).is_err() {
                dst.copy_from_slice(src);
            }
        }
        if full < pcm.len() {
            out[full..].copy_from_slice(&pcm[full..]);
        }
        out
    }
}

fn map_ns_level(level: u8) -> NoiseSuppressionLevel {
    match level {
        0 => NoiseSuppressionLevel::Low,
        1 => NoiseSuppressionLevel::Moderate,
        2 => NoiseSuppressionLevel::High,
        _ => NoiseSuppressionLevel::VeryHigh,
    }
}

fn build_sonora_apm(opts: AudioProcessingOptions) -> SonoraApm {
    let cfg = build_sonora_config(opts);
    let stream = StreamConfig::new(APM_SAMPLE_RATE, 1);
    let mut aec3 = EchoCanceller3Config::default();
    // Match Zalo's `audioEchoComfortNoise: false`: keep synthetic comfort noise
    // floor silent so sustained speech/background noise never turns into hiss.
    aec3.comfort_noise.noise_floor_dbfs = -200.0;
    SonoraApm::builder()
        .config(cfg)
        .echo_canceller3_config(aec3)
        .capture_config(stream)
        .render_config(stream)
        .build()
}

fn build_sonora_config(opts: AudioProcessingOptions) -> Config {
    let echo_canceller = opts.aec.then_some(EchoCanceller {
        enforce_high_pass_filtering: opts.high_pass,
        // HMM transparent mode detects headsets with no acoustic echo path
        // and avoids suppressing clean near-end speech.
        transparent_mode: TransparentModeType::Hmm,
    });
    let noise_suppression = opts.noise_suppression.then(|| NoiseSuppression {
        level: map_ns_level(opts.ns_level),
        analyze_linear_aec_output_when_available: false,
    });
    let high_pass_filter = opts.high_pass.then(HighPassFilter::default);
    let gain_controller2 = opts.gain_control.then(|| {
        // Match Zalo's clean vocal loudness without double-AGC distortion:
        // start at 0 dB (so hot mics never clip), use RNN-VAD adaptive digital
        // gain capped cleanly with noise floor ceiling -48 dBFS and built-in
        // -5 dB headroom limiter.
        let max_gain = f32::from(opts.agc_gain_db).clamp(9.0, 18.0);
        GainController2 {
            input_volume_controller: false,
            adaptive_digital: Some(AdaptiveDigital {
                headroom_db: 5.0,
                max_gain_db: max_gain,
                initial_gain_db: 0.0,
                max_gain_change_db_per_second: 6.0,
                max_output_noise_level_dbfs: -48.0,
            }),
            fixed_digital: FixedDigital { gain_db: 0.0 },
        }
    });
    Config {
        echo_canceller,
        noise_suppression,
        high_pass_filter,
        gain_controller2,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone_frame(freq_hz: f64, amp: f64, phase: &mut f64) -> Vec<i16> {
        let step = 2.0 * std::f64::consts::PI * freq_hz / f64::from(APM_SAMPLE_RATE);
        (0..320)
            .map(|_| {
                let s = (amp * phase.sin()).round().clamp(-32767.0, 32767.0) as i16;
                *phase += step;
                s
            })
            .collect()
    }

    fn rms(pcm: &[i16]) -> f64 {
        let sum: f64 = pcm.iter().map(|&v| f64::from(v) * f64::from(v)).sum();
        (sum / pcm.len().max(1) as f64).sqrt()
    }

    #[test]
    fn processes_20ms_frames_and_reconfigures_live() {
        let apm = InProcessApm::new(AudioProcessingOptions::default());
        assert!(apm.is_enabled());
        let mut phase = 0.0;
        let render = tone_frame(440.0, 4000.0, &mut phase);
        apm.feed_render(&render, 60);
        let out = apm.process_capture(&render, 20);
        assert_eq!(out.len(), 320);

        let off = AudioProcessingOptions {
            aec: false,
            noise_suppression: false,
            ns_level: 0,
            high_pass: false,
            gain_control: false,
            agc_gain_db: 0,
            voice_detection: false,
        };
        apm.apply_options(off);
        assert!(!apm.is_enabled());
        assert_eq!(apm.process_capture(&render, 20), render);

        // Resetting back to default options re-enables DSP cleanly.
        apm.reset(AudioProcessingOptions::default());
        assert!(apm.is_enabled());
        assert_eq!(apm.process_capture(&render, 20).len(), 320);
    }

    #[test]
    fn aec3_attenuates_linear_echo_after_convergence() {
        let opts = AudioProcessingOptions {
            aec: true,
            noise_suppression: false,
            ns_level: 2,
            high_pass: false,
            gain_control: false,
            agc_gain_db: 9,
            voice_detection: false,
        };
        let apm = InProcessApm::new(opts);
        // Pseudo-random speech-like far-end excitation so AEC3's adaptive
        // filter converges across frequency bins.
        let mut state = 0x1234_5678_u32;
        let mut next_frame = || {
            let mut f = Vec::with_capacity(320);
            for _ in 0..320 {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let s = ((state >> 16) as i16) / 3;
                f.push(s);
            }
            f
        };
        let mut delay_line: std::collections::VecDeque<Vec<i16>> =
            std::collections::VecDeque::new();
        for _ in 0..3 {
            delay_line.push_back(vec![0_i16; 320]);
        }
        let mut in_rms = 0.0;
        let mut out_rms = 0.0;
        for i in 0..120 {
            let far = next_frame();
            apm.feed_render(&far, 60);
            delay_line.push_back(far);
            let delayed = delay_line.pop_front().unwrap();
            let echo: Vec<i16> = delayed.iter().map(|&s| s / 2).collect();
            let cleaned = apm.process_capture(&echo, 0);
            if i >= 100 {
                in_rms += rms(&echo);
                out_rms += rms(&cleaned);
            }
        }
        assert!(
            out_rms < in_rms * 0.35,
            "AEC3 should attenuate steady echo strongly: in_rms={in_rms:.1}, out_rms={out_rms:.1}"
        );
    }

    #[test]
    fn default_apm_preserves_continuous_speech_without_dropout() {
        let apm = InProcessApm::new(AudioProcessingOptions::default());
        let silence = vec![0_i16; 320];
        let mut in_sum = 0.0;
        let mut out_sum = 0.0;
        for f in 0..250 {
            let pcm: Vec<i16> = (0..320)
                .map(|s| {
                    let t = (f * 320 + s) as f64 / 16_000.0;
                    let env = 0.35 + 0.65 * (2.0 * std::f64::consts::PI * 3.5 * t).sin().abs();
                    let sig = 0.5 * (2.0 * std::f64::consts::PI * 145.0 * t).sin()
                        + 0.3 * (2.0 * std::f64::consts::PI * 435.0 * t).sin()
                        + 0.15 * (2.0 * std::f64::consts::PI * 1160.0 * t).sin();
                    (env * sig * 10_000.0).clamp(-32767.0, 32767.0) as i16
                })
                .collect();
            apm.feed_render(&silence, 60);
            let out = apm.process_capture(&pcm, 20);
            if f >= 100 {
                in_sum += rms(&pcm);
                out_sum += rms(&out);
            }
        }
        assert!(
            out_sum > in_sum * 0.85,
            "default APM must preserve continuous speech: in={in_sum:.1}, out={out_sum:.1}"
        );
    }
}
