//! In-process Audio Processing: WebRTC AEC3 + WebRTC Noise Suppression
//! (`sonora-ns`, leveled `0..=3`) + smooth WebRTC AGC2/limiter, matching Zalo
//! Windows (`ZaloCall.exe`), macOS (`ZaloCall`), and Android (`libzrtc.so`).
//!
//! Supports running natively at **48 kHz** (matching PipeWire, CoreAudio, and
//! WASAPI hardware clocks without kernel/daemon quantum resampling) or **16 kHz**
//! (fallback / direct 16 kHz paths). Optional native 48 kHz RNNoise
//! (`nnnoiseless`) is available as an opt-in (`ZCALL_AI_NS=1`) and is never fed
//! band-limited 16 kHz upsampled audio.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;

use nnnoiseless::DenoiseState;
use sonora::config::{
    AdaptiveDigital, EchoCanceller, EchoCanceller3Config, FixedDigital, GainController2,
    HighPassFilter, MaxProcessingRate, NoiseSuppression, NoiseSuppressionLevel, Pipeline,
    TransparentModeType,
};
use sonora::{AudioProcessing as SonoraApm, Config, StreamConfig};
use zcall_core::zrtc_helpers::{ai_noise_suppression_requested, AudioProcessingOptions};

/// Legacy 16 kHz call sample rate (Hz).
pub const APM_SAMPLE_RATE: u32 = 16_000;
/// Native hardware sample rate for PipeWire / CoreAudio / WASAPI (Hz).
pub const APM_NATIVE_RATE: u32 = 48_000;
/// Samples per WebRTC 10 ms subframe at 16 kHz mono.
pub const APM_SUBFRAME_SAMPLES: usize = 160;
/// Samples per RNNoise 10 ms subframe at 48 kHz mono.
const RNNOISE_FRAME_SAMPLES: usize = DenoiseState::FRAME_SIZE;
/// Default speaker latency estimate (ms) before the first hardware measurement.
const DEFAULT_PLAY_LATENCY_MS: u32 = 80;
/// Default capture latency estimate (ms).
const DEFAULT_CAP_LATENCY_MS: u32 = 20;

/// Native 48 kHz RNNoise stage (only used when the capture stream itself is
/// native 48 kHz and `ZCALL_AI_NS=1` is enabled).
struct NativeRnnoise48k {
    state: Box<DenoiseState<'static>>,
    in_48k: [f32; RNNOISE_FRAME_SAMPLES],
    out_48k: [f32; RNNOISE_FRAME_SAMPLES],
}

impl NativeRnnoise48k {
    fn new() -> Self {
        Self {
            state: DenoiseState::new(),
            in_48k: [0.0; RNNOISE_FRAME_SAMPLES],
            out_48k: [0.0; RNNOISE_FRAME_SAMPLES],
        }
    }

    fn process_subframe(&mut self, src: &[i16], dst: &mut [i16]) {
        debug_assert_eq!(src.len(), RNNOISE_FRAME_SAMPLES);
        debug_assert_eq!(dst.len(), RNNOISE_FRAME_SAMPLES);
        for (slot, &s) in self.in_48k.iter_mut().zip(src.iter()) {
            *slot = f32::from(s);
        }
        let _vad = self.state.process_frame(&mut self.out_48k, &self.in_48k);
        for (slot, &s) in dst.iter_mut().zip(self.out_48k.iter()) {
            *slot = s.round().clamp(-32_768.0, 32_767.0) as i16;
        }
    }
}

/// Exponential moving average tracker (Q8 fixed-point ms) for smooth AEC3 delay
/// estimates without sudden step jumps.
struct LatencySmoother {
    /// Stored as `ms * 256`.
    q8: AtomicU32,
}

impl LatencySmoother {
    fn new(initial_ms: u32) -> Self {
        Self {
            q8: AtomicU32::new(initial_ms.min(500) << 8),
        }
    }

    fn reset(&self, initial_ms: u32) {
        self.q8
            .store(initial_ms.min(500) << 8, Ordering::Relaxed);
    }

    /// Updates the EMA (`alpha = 0.25`) and returns the smoothed ms.
    fn update(&self, measured_ms: u32, cap_ms: u32) -> u32 {
        let clamped = measured_ms.min(cap_ms) << 8;
        let prev = self.q8.load(Ordering::Relaxed);
        let next = (prev * 3 + clamped + 2) >> 2;
        self.q8.store(next, Ordering::Relaxed);
        (next + 128) >> 8
    }

    fn get_ms(&self) -> u32 {
        (self.q8.load(Ordering::Relaxed) + 128) >> 8
    }
}

/// Thread-safe in-process WebRTC APM shared between playout (`zcall-play`) and
/// capture (`zcall-mic`).
pub struct InProcessApm {
    sample_rate: u32,
    subframe_samples: usize,
    inner: Mutex<SonoraApm>,
    rnnoise: Mutex<Option<NativeRnnoise48k>>,
    play_latency: LatencySmoother,
    cap_latency: LatencySmoother,
    enabled: AtomicBool,
    aec_enabled: AtomicBool,
    ai_ns_enabled: AtomicBool,
    sonora_capture_enabled: AtomicBool,
    gain_control: AtomicBool,
}

impl InProcessApm {
    /// Builds a 16 kHz mono APM configured from `zrtcConfig` (backwards-compatible default).
    #[must_use]
    pub fn new(opts: AudioProcessingOptions) -> Self {
        Self::with_sample_rate(APM_SAMPLE_RATE, opts)
    }

    /// Builds a mono APM running natively at `sample_rate` (48_000 Hz recommended
    /// for PipeWire / CoreAudio / WASAPI, or 16_000 Hz for 16 kHz streams).
    #[must_use]
    pub fn with_sample_rate(sample_rate: u32, opts: AudioProcessingOptions) -> Self {
        let rate = match sample_rate {
            48_000 | 32_000 | 16_000 => sample_rate,
            _ => APM_SAMPLE_RATE,
        };
        let subframe_samples = (rate as usize) / 100;
        let use_ai_ns = rate == APM_NATIVE_RATE && ai_noise_suppression_requested();
        let apm = build_sonora_apm(rate, opts, use_ai_ns);
        let active = opts.aec || opts.noise_suppression || opts.gain_control || opts.high_pass;
        let sonora_cap = opts.aec
            || opts.gain_control
            || opts.high_pass
            || (opts.noise_suppression && !use_ai_ns);
        let rnnoise = (opts.noise_suppression && use_ai_ns).then(NativeRnnoise48k::new);
        Self {
            sample_rate: rate,
            subframe_samples,
            inner: Mutex::new(apm),
            rnnoise: Mutex::new(rnnoise),
            play_latency: LatencySmoother::new(DEFAULT_PLAY_LATENCY_MS),
            cap_latency: LatencySmoother::new(DEFAULT_CAP_LATENCY_MS),
            enabled: AtomicBool::new(active),
            aec_enabled: AtomicBool::new(opts.aec),
            ai_ns_enabled: AtomicBool::new(opts.noise_suppression && use_ai_ns),
            sonora_capture_enabled: AtomicBool::new(sonora_cap),
            gain_control: AtomicBool::new(opts.gain_control),
        }
    }

    /// Configured native sample rate in Hz (16_000, 32_000, or 48_000).
    #[must_use]
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Samples per 10 ms subframe at [`Self::sample_rate`].
    #[must_use]
    pub fn subframe_samples(&self) -> usize {
        self.subframe_samples
    }

    /// Samples per 20 ms Opus-aligned frame at [`Self::sample_rate`].
    #[must_use]
    pub fn frame_samples_20ms(&self) -> usize {
        self.subframe_samples * 2
    }

    /// Returns whether at least one DSP stage is currently enabled.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }

    /// Returns whether AEC3 is currently enabled.
    #[must_use]
    pub fn has_aec(&self) -> bool {
        self.aec_enabled.load(Ordering::Acquire)
    }

    /// Returns whether native 48 kHz AI noise suppression (RNNoise) is enabled.
    #[must_use]
    pub fn has_rnnoise(&self) -> bool {
        self.ai_ns_enabled.load(Ordering::Acquire)
    }

    /// Returns whether sonora's smooth AGC2 + limiter stage is enabled.
    #[must_use]
    pub fn has_gain_control(&self) -> bool {
        self.gain_control.load(Ordering::Acquire)
    }

    /// Reconfigures AEC / NS / HPF / AGC live in-place without reopening mic
    /// or speaker streams.
    pub fn apply_options(&self, opts: AudioProcessingOptions) {
        let use_ai_ns = self.sample_rate == APM_NATIVE_RATE && ai_noise_suppression_requested();
        let active = opts.aec || opts.noise_suppression || opts.gain_control || opts.high_pass;
        let sonora_cap = opts.aec
            || opts.gain_control
            || opts.high_pass
            || (opts.noise_suppression && !use_ai_ns);
        self.enabled.store(active, Ordering::Release);
        self.aec_enabled.store(opts.aec, Ordering::Release);
        self.ai_ns_enabled
            .store(opts.noise_suppression && use_ai_ns, Ordering::Release);
        self.sonora_capture_enabled
            .store(sonora_cap, Ordering::Release);
        self.gain_control.store(opts.gain_control, Ordering::Release);
        let cfg = build_sonora_config(self.sample_rate, opts, use_ai_ns);
        {
            let mut apm = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            apm.apply_config(cfg);
        }
        if opts.noise_suppression && use_ai_ns {
            let mut rnn = self.rnnoise.lock().unwrap_or_else(|e| e.into_inner());
            if rnn.is_none() {
                *rnn = Some(NativeRnnoise48k::new());
            }
        }
    }

    /// Rebuilds the internal DSP state cleanly (for new calls or hardware
    /// mic/speaker hot-swaps where the acoustic echo path changes).
    pub fn reset(&self, opts: AudioProcessingOptions) {
        let use_ai_ns = self.sample_rate == APM_NATIVE_RATE && ai_noise_suppression_requested();
        let active = opts.aec || opts.noise_suppression || opts.gain_control || opts.high_pass;
        let sonora_cap = opts.aec
            || opts.gain_control
            || opts.high_pass
            || (opts.noise_suppression && !use_ai_ns);
        self.enabled.store(active, Ordering::Release);
        self.aec_enabled.store(opts.aec, Ordering::Release);
        self.ai_ns_enabled
            .store(opts.noise_suppression && use_ai_ns, Ordering::Release);
        self.sonora_capture_enabled
            .store(sonora_cap, Ordering::Release);
        self.gain_control.store(opts.gain_control, Ordering::Release);
        self.play_latency.reset(DEFAULT_PLAY_LATENCY_MS);
        self.cap_latency.reset(DEFAULT_CAP_LATENCY_MS);
        let fresh = build_sonora_apm(self.sample_rate, opts, use_ai_ns);
        {
            let mut apm = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            *apm = fresh;
        }
        {
            let mut rnn = self.rnnoise.lock().unwrap_or_else(|e| e.into_inner());
            *rnn = (opts.noise_suppression && use_ai_ns).then(NativeRnnoise48k::new);
        }
    }

    /// Feeds one played (far-end) PCM buffer (at [`Self::sample_rate`], typically
    /// 20 ms = 960 samples @ 48 kHz or 320 samples @ 16 kHz) into AEC3's reverse
    /// stream along with the speaker's measured output latency in milliseconds.
    pub fn feed_render(&self, pcm: &[i16], play_latency_ms: u32) {
        self.play_latency.update(play_latency_ms, 400);
        if !self.aec_enabled.load(Ordering::Acquire) || pcm.len() < self.subframe_samples {
            return;
        }
        let mut apm = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut scratch = vec![0_i16; self.subframe_samples];
        for chunk in pcm.chunks_exact(self.subframe_samples) {
            let _ = apm.process_render_i16(chunk, &mut scratch);
        }
    }

    /// Processes one captured (near-end) PCM buffer at [`Self::sample_rate`]
    /// through WebRTC AEC3 → WebRTC NS → WebRTC AGC2/limiter (plus opt-in native
    /// 48 kHz RNNoise when requested).
    pub fn process_capture(&self, pcm: &[i16], cap_latency_ms: u32) -> Vec<i16> {
        self.process_capture_with_aec(pcm, cap_latency_ms, true)
    }

    /// Same as [`Self::process_capture`], but allows bypassing WebRTC AEC3
    /// (for Bluetooth HFP headsets with on-chip AEC) while keeping NS and AGC2
    /// active.
    pub fn process_capture_with_aec(
        &self,
        pcm: &[i16],
        cap_latency_ms: u32,
        allow_aec: bool,
    ) -> Vec<i16> {
        let sub = self.subframe_samples;
        if !self.enabled.load(Ordering::Acquire) || pcm.len() < sub {
            return pcm.to_vec();
        }
        let full = (pcm.len() / sub) * sub;
        let run_sonora = self.sonora_capture_enabled.load(Ordering::Acquire)
            && (allow_aec || !self.aec_enabled.load(Ordering::Acquire));
        let run_ai_ns = self.ai_ns_enabled.load(Ordering::Acquire);
        if !run_sonora && !run_ai_ns {
            return pcm.to_vec();
        }

        let mut mid = if run_sonora {
            let cap_ms = self.cap_latency.update(cap_latency_ms, 200);
            let play_ms = self.play_latency.get_ms();
            let total_delay_ms = play_ms.saturating_add(cap_ms).min(500) as i32;
            let mut apm = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            let _ = apm.set_stream_delay_ms(total_delay_ms);
            let mut out = vec![0_i16; pcm.len()];
            for (src, dst) in pcm[..full]
                .chunks_exact(sub)
                .zip(out[..full].chunks_exact_mut(sub))
            {
                if apm.process_capture_i16(src, dst).is_err() {
                    dst.copy_from_slice(src);
                }
            }
            if full < pcm.len() {
                out[full..].copy_from_slice(&pcm[full..]);
            }
            out
        } else {
            pcm.to_vec()
        };

        if run_ai_ns && sub == RNNOISE_FRAME_SAMPLES {
            let mut rnn_guard = self.rnnoise.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(rnn) = rnn_guard.as_mut() {
                let mut out = vec![0_i16; mid.len()];
                for (src, dst) in mid[..full]
                    .chunks_exact(RNNOISE_FRAME_SAMPLES)
                    .zip(out[..full].chunks_exact_mut(RNNOISE_FRAME_SAMPLES))
                {
                    rnn.process_subframe(src, dst);
                }
                if full < mid.len() {
                    out[full..].copy_from_slice(&mid[full..]);
                }
                mid = out;
            }
        }
        mid
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

fn build_sonora_apm(sample_rate: u32, opts: AudioProcessingOptions, use_ai_ns: bool) -> SonoraApm {
    let cfg = build_sonora_config(sample_rate, opts, use_ai_ns);
    let stream = StreamConfig::new(sample_rate, 1);
    let mut aec3 = EchoCanceller3Config::default();
    // Keep a natural quiet comfort-noise floor (-70 dBFS instead of -200 dBFS
    // hard digital mute) so double-talk and echo suppression never chop or hiss.
    aec3.comfort_noise.noise_floor_dbfs = -70.0;
    SonoraApm::builder()
        .config(cfg)
        .echo_canceller3_config(aec3)
        .capture_config(stream)
        .render_config(stream)
        .build()
}

fn build_sonora_config(sample_rate: u32, opts: AudioProcessingOptions, use_ai_ns: bool) -> Config {
    let pipeline = Pipeline {
        maximum_internal_processing_rate: if sample_rate >= APM_NATIVE_RATE {
            MaxProcessingRate::Rate48kHz
        } else {
            MaxProcessingRate::Rate32kHz
        },
        ..Default::default()
    };
    let echo_canceller = opts.aec.then_some(EchoCanceller {
        // Always enforce AEC3's internal DC/80 Hz high-pass filter so hardware
        // DC offset and 50/60 Hz mains hum cannot saturate the echo filter.
        enforce_high_pass_filtering: true,
        // HMM transparent mode detects headsets with no acoustic echo path
        // and avoids suppressing clean near-end speech.
        transparent_mode: TransparentModeType::Hmm,
    });
    let noise_suppression = (opts.noise_suppression && !use_ai_ns).then(|| NoiseSuppression {
        level: map_ns_level(opts.ns_level),
        analyze_linear_aec_output_when_available: false,
    });
    let high_pass_filter = opts.high_pass.then(HighPassFilter::default);
    let gain_controller2 = opts.gain_control.then(|| {
        // Single smooth WebRTC digital gain + lookahead limiter stage:
        // replaces MicLeveler's per-frame step limiter (which caused 20 ms
        // boundary clicks) while keeping noise floor below -52 dBFS.
        let max_gain = f32::from(opts.agc_gain_db).clamp(6.0, 12.0);
        GainController2 {
            input_volume_controller: false,
            adaptive_digital: Some(AdaptiveDigital {
                headroom_db: 6.0,
                max_gain_db: max_gain,
                initial_gain_db: 0.0,
                max_gain_change_db_per_second: 3.0,
                max_output_noise_level_dbfs: -52.0,
            }),
            fixed_digital: FixedDigital { gain_db: 0.0 },
        }
    });
    Config {
        pipeline,
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

    fn tone_frame(rate: u32, freq_hz: f64, amp: f64, phase: &mut f64) -> Vec<i16> {
        let len = (rate as usize) / 50; // 20 ms
        let step = 2.0 * std::f64::consts::PI * freq_hz / f64::from(rate);
        (0..len)
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
    fn processes_48k_and_16k_frames_and_reconfigures_live() {
        for rate in [APM_NATIVE_RATE, APM_SAMPLE_RATE] {
            let apm = InProcessApm::with_sample_rate(rate, AudioProcessingOptions::default());
            assert!(apm.is_enabled());
            assert!(apm.has_aec());
            assert!(apm.has_gain_control());
            assert!(!apm.has_rnnoise());
            let mut phase = 0.0;
            let render = tone_frame(rate, 440.0, 4000.0, &mut phase);
            apm.feed_render(&render, 80);
            let out = apm.process_capture(&render, 20);
            assert_eq!(out.len(), apm.frame_samples_20ms());

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
            assert!(!apm.has_aec());
            assert!(!apm.has_gain_control());
            assert_eq!(apm.process_capture(&render, 20), render);

            apm.reset(AudioProcessingOptions::default());
            assert!(apm.is_enabled());
            assert!(apm.has_gain_control());
            assert_eq!(apm.process_capture(&render, 20).len(), apm.frame_samples_20ms());
        }
    }

    #[test]
    fn aec3_attenuates_linear_echo_at_48khz_after_convergence() {
        let opts = AudioProcessingOptions {
            aec: true,
            noise_suppression: false,
            ns_level: 2,
            high_pass: false,
            gain_control: false,
            agc_gain_db: 9,
            voice_detection: false,
        };
        let apm = InProcessApm::with_sample_rate(APM_NATIVE_RATE, opts);
        let frame_len = apm.frame_samples_20ms();
        let mut state = 0x1234_5678_u32;
        let mut next_frame = || {
            let mut f = Vec::with_capacity(frame_len);
            for _ in 0..frame_len {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                f.push(((state >> 16) as i16) / 3);
            }
            f
        };
        let mut delay_line: std::collections::VecDeque<Vec<i16>> =
            std::collections::VecDeque::new();
        for _ in 0..3 {
            delay_line.push_back(vec![0_i16; frame_len]);
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
            "48 kHz AEC3 should attenuate steady echo strongly: in_rms={in_rms:.1}, out_rms={out_rms:.1}"
        );
    }

    #[test]
    fn default_apm_preserves_continuous_speech_including_sibilants_without_dropout() {
        let apm = InProcessApm::with_sample_rate(APM_NATIVE_RATE, AudioProcessingOptions::default());
        let frame_len = apm.frame_samples_20ms();
        let silence = vec![0_i16; frame_len];
        let mut phase = 0.0_f64;
        let mut rng = 0xDEAD_BEEF_u32;
        let mut in_sum = 0.0;
        let mut out_sum = 0.0;
        let mut prev_active = false;
        for f in 0..300 {
            let t0 = (f * frame_len) as f64 / f64::from(APM_NATIVE_RATE);
            let cycle = t0 % 1.5;
            let active = (0.35..1.35).contains(&cycle);
            let mut pcm = Vec::with_capacity(frame_len);
            for s in 0..frame_len {
                let t = (f * frame_len + s) as f64 / f64::from(APM_NATIVE_RATE);
                rng = rng.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let hiss = (((rng >> 16) as i16) as f64 / 32_768.0) * 180.0;
                let voice = if active {
                    let f0 = 135.0
                        + 45.0 * (2.0 * std::f64::consts::PI * 1.3 * t).sin()
                        + 25.0 * (2.0 * std::f64::consts::PI * 2.9 * t).cos();
                    phase += 2.0 * std::f64::consts::PI * f0 / f64::from(APM_NATIVE_RATE);
                    let env = 0.25 + 0.75 * (2.0 * std::f64::consts::PI * 4.2 * t).sin().abs();
                    let harmonics = 0.45 * phase.sin()
                        + 0.30 * (2.0 * phase).sin()
                        + 0.15 * (3.0 * phase).sin()
                        + 0.10 * (8.0 * phase).sin();
                    env * harmonics * 7_500.0
                } else {
                    0.0
                };
                pcm.push((voice + hiss).clamp(-32_767.0, 32_767.0) as i16);
            }
            apm.feed_render(&silence, 80);
            let out = apm.process_capture(&pcm, 20);
            if f >= 100 && active && prev_active {
                in_sum += rms(&pcm);
                out_sum += rms(&out);
            }
            prev_active = active;
        }
        assert!(
            out_sum > in_sum * 0.75,
            "default 48 kHz WebRTC APM must preserve natural speech without dropout: in={in_sum:.1}, out={out_sum:.1}"
        );
    }

    #[test]
    fn default_webrtc_ns_suppresses_stationary_broadband_noise() {
        let opts = AudioProcessingOptions {
            aec: false,
            noise_suppression: true,
            ns_level: 2,
            high_pass: false,
            gain_control: false,
            agc_gain_db: 9,
            voice_detection: false,
        };
        let apm = InProcessApm::with_sample_rate(APM_NATIVE_RATE, opts);
        let frame_len = apm.frame_samples_20ms();
        let mut state = 0xCAFE_BABE_u32;
        let mut in_rms = 0.0;
        let mut out_rms = 0.0;
        for i in 0..100 {
            let noise: Vec<i16> = (0..frame_len)
                .map(|_| {
                    state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    let u = ((state >> 16) as i16) as f64 / 32768.0;
                    (u * 2_500.0) as i16
                })
                .collect();
            let cleaned = apm.process_capture(&noise, 20);
            if i >= 60 {
                in_rms += rms(&noise);
                out_rms += rms(&cleaned);
            }
        }
        assert!(
            out_rms < in_rms * 0.45,
            "WebRTC NS should attenuate stationary broadband noise: in={in_rms:.1}, out={out_rms:.1}"
        );
    }

    #[test]
    fn agc2_keeps_frame_boundaries_smooth_under_loud_peaks() {
        let opts = AudioProcessingOptions {
            aec: false,
            noise_suppression: false,
            ns_level: 0,
            high_pass: false,
            gain_control: true,
            agc_gain_db: 9,
            voice_detection: false,
        };
        let apm = InProcessApm::with_sample_rate(APM_NATIVE_RATE, opts);
        let mut phase = 0.0;
        let mut prev_tail = 0_i16;
        let mut max_boundary_jump = 0_i32;
        for f in 0..40 {
            // Alternate normal speech level and sudden loud peak frames.
            let amp = if f % 2 == 0 { 6_000.0 } else { 31_000.0 };
            let frame = tone_frame(APM_NATIVE_RATE, 300.0, amp, &mut phase);
            let out = apm.process_capture(&frame, 20);
            if f > 0 {
                let jump = (i32::from(out[0]) - i32::from(prev_tail)).abs();
                max_boundary_jump = max_boundary_jump.max(jump);
            }
            prev_tail = *out.last().unwrap_or(&0);
        }
        // At 300 Hz / 48 kHz, max natural sample-to-sample step of a 31_000 sine
        // is ~1217; any step-limiter click would jump by > 4000.
        assert!(
            max_boundary_jump < 2_500,
            "AGC2 limiter must not introduce 20 ms frame-boundary clicks: max_jump={max_boundary_jump}"
        );
    }
}
