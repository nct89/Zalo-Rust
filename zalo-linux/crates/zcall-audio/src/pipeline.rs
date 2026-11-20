//! The 1-1 audio pipeline: mic frame → ClipGuard decision + MicLeveler gain
//! → Opus encode → the media layer. This ties the already-ported pieces
//! together, mirroring `encoder_loop` in the reference call implementation
//! (`audio-io.py`).

use crate::capture::MicCapture;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;
use zcall_core::audio::{OpusEncoder, OpusEncoderConfig};
use zcall_core::clip_guard::{ClipGuard, StepKind};
use zcall_core::leveler::MicLeveler;

/// A callback receiving one encoded Opus frame.
type EncodedCallback = Box<dyn FnMut(&[u8])>;

/// The capture → encode side of a call.
pub struct AudioPipeline {
    /// Opus encoder (16 kHz mono 20 ms).
    pub encoder: OpusEncoder,
    /// Soft make-up gain.
    pub leveler: MicLeveler,
    /// OS-volume clip guard. Shared with the thread that runs `pactl`.
    pub clip_guard: Arc<Mutex<ClipGuard>>,
    /// When muted, no frame is encoded.
    pub muted: bool,
    /// The latest step `feed` asked for. The caller applies it and clears it.
    pending_clip: Option<StepKind>,
    on_encoded: Option<EncodedCallback>,
}

impl AudioPipeline {
    /// Builds the pipeline with default 1-1 Opus settings.
    ///
    /// # Errors
    ///
    /// Returns the Opus error when the encoder cannot be created.
    pub fn new(high_pass: bool) -> Result<Self, zcall_core::audio::OpusError> {
        Self::with_config(high_pass, OpusEncoderConfig::default())
    }

    /// Builds the pipeline with explicit Opus encoder configuration.
    ///
    /// # Errors
    ///
    /// Returns the Opus error when the encoder cannot be created.
    pub fn with_config(
        high_pass: bool,
        opus: OpusEncoderConfig,
    ) -> Result<Self, zcall_core::audio::OpusError> {
        Ok(Self {
            encoder: OpusEncoder::with_config(opus)?,
            leveler: MicLeveler::new(high_pass),
            clip_guard: Arc::new(Mutex::new(ClipGuard::new(true))),
            muted: false,
            pending_clip: None,
            on_encoded: None,
        })
    }

    /// Updates Opus bitrate / complexity / FEC settings in place.
    ///
    /// # Errors
    ///
    /// Returns the Opus error when libopus rejects the configuration.
    pub fn apply_encoder_config(
        &mut self,
        opus: OpusEncoderConfig,
    ) -> Result<(), zcall_core::audio::OpusError> {
        self.encoder.apply_config(opus)
    }

    /// The guard `process_frame` feeds. The volume thread calls `step` on it.
    pub fn clip_guard(&self) -> Arc<Mutex<ClipGuard>> {
        Arc::clone(&self.clip_guard)
    }

    /// Takes the clip-guard step from the last frame, if it asked for one.
    pub fn take_clip_step(&mut self) -> Option<StepKind> {
        self.pending_clip.take()
    }

    /// Installs the callback that receives each encoded Opus frame.
    pub fn set_on_encoded<F>(&mut self, callback: F)
    where
        F: FnMut(&[u8]) + 'static,
    {
        self.on_encoded = Some(Box::new(callback));
    }

    /// Updates expected uplink packet loss percentage (when FEC is active).
    ///
    /// # Errors
    ///
    /// Returns the Opus error when libopus rejects the value.
    pub fn set_packet_loss_perc(
        &mut self,
        loss_perc: i32,
    ) -> Result<(), zcall_core::audio::OpusError> {
        self.encoder.set_packet_loss_perc(loss_perc)
    }

    /// Encodes one 320-sample frame already processed by in-process WebRTC APM
    /// (`InProcessApm`), skipping the redundant software `MicLeveler` so only a
    /// single AGC/limiter stage runs (matching Windows `ZaloCall.exe`).
    pub fn process_apm_frame(
        &mut self,
        processed_pcm: &[i16],
        muted: bool,
    ) -> Option<(Vec<u8>, Vec<i16>)> {
        let tx_pcm = if muted {
            vec![0_i16; zcall_core::audio::FRAME_SAMPLES]
        } else {
            processed_pcm.to_vec()
        };
        let opus = self.encoder.encode_frame(&tx_pcm).ok()?;
        Some((opus, tx_pcm))
    }

    /// Processes one 320-sample frame using raw capture samples for ClipGuard
    /// and scaled samples for MicLeveler/Opus, keeping MicLeveler calibrated
    /// while sending silence when muted.
    ///
    /// Returns `(encoded_opus, encoded_pcm)` so RFC 6464 audio level reflects
    /// the post-leveler frame actually sent to the encoder.
    pub fn process_mic_frame(
        &mut self,
        raw_clip: &[i16],
        pcm: &[i16],
        muted: bool,
        far_active: bool,
        now: f64,
    ) -> Option<(Vec<u8>, Vec<i16>)> {
        if !muted {
            let step = self
                .clip_guard
                .lock()
                .unwrap_or_else(|err| err.into_inner())
                .feed(raw_clip, now);
            if let Some(step) = step {
                self.pending_clip = Some(step);
            }
        }
        let gained = self.leveler.process(pcm, far_active);
        let tx_pcm = if muted {
            vec![0_i16; zcall_core::audio::FRAME_SAMPLES]
        } else {
            gained
        };
        let opus = self.encoder.encode_frame(&tx_pcm).ok()?;
        Some((opus, tx_pcm))
    }

    /// Processes one 320-sample frame.
    ///
    /// Returns the encoded Opus frame, or `None` when muted or encoding fails.
    pub fn process_frame(&mut self, pcm: &[i16], far_active: bool, now: f64) -> Option<Vec<u8>> {
        if self.muted {
            return None;
        }
        // `step` (the pactl write) runs on another thread, on this same guard.
        let step = self
            .clip_guard
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .feed(pcm, now);
        if let Some(step) = step {
            self.pending_clip = Some(step);
        }
        let gained = self.leveler.process(pcm, far_active);
        self.encoder.encode_frame(&gained).ok()
    }

    /// Encodes a frame and hands it to the callback.
    pub fn send_frame(&mut self, pcm: &[i16], far_active: bool, now: f64) -> Option<Vec<u8>> {
        let frame = self.process_frame(pcm, far_active, now)?;
        if let Some(callback) = self.on_encoded.as_mut() {
            callback(&frame);
        }
        Some(frame)
    }

    /// Runs the mic → pipeline → callback loop until `stop` is set.
    pub fn run(&mut self, mic: &mut MicCapture, stop: &AtomicBool) {
        while !stop.load(Ordering::SeqCst) {
            let Some(frame) = mic.next_frame() else {
                break;
            };
            let samples = bytes_to_i16(&frame);
            self.send_frame(&samples, false, monotonic_secs());
        }
    }
}

/// Interprets little-endian i16 PCM bytes.
pub fn bytes_to_i16(bytes: &[u8]) -> Vec<i16> {
    bytes
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect()
}

/// Seconds since this process started.
///
/// `Instant::now().elapsed()` is about zero on every call: that Instant was
/// just created. ClipGuard's one-second gap never elapses on that clock.
fn monotonic_secs() -> f64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64()
}

#[cfg(test)]
mod tests {
    use super::*;
    use zcall_core::audio::OpusDecoder;

    fn sine(amplitude: i16, samples: usize) -> Vec<i16> {
        (0..samples)
            .map(|i| {
                let t = i as f64 / 16_000.0;
                (f64::from(amplitude) * (2.0 * std::f64::consts::PI * 440.0 * t).sin()) as i16
            })
            .collect()
    }

    #[test]
    fn encodes_a_speech_frame() {
        let mut pipeline = AudioPipeline::new(false).expect("pipeline");
        let encoded = pipeline
            .process_frame(&sine(2000, 320), false, 0.0)
            .expect("encoded");
        assert!(!encoded.is_empty());
    }

    #[test]
    fn muted_pipeline_produces_nothing() {
        let mut pipeline = AudioPipeline::new(false).expect("pipeline");
        pipeline.muted = true;
        assert!(pipeline
            .process_frame(&sine(2000, 320), false, 0.0)
            .is_none());
    }

    #[test]
    fn raises_quiet_speech_toward_the_target() {
        let mut pipeline = AudioPipeline::new(false).expect("pipeline");
        let mut decoder = OpusDecoder::new().expect("decoder");
        // Alternate speech / pause so the leveler learns the noise floor.
        for _ in 0..40 {
            for _ in 0..3 {
                let encoded = pipeline.process_frame(&sine(800, 320), false, 0.0).unwrap();
                let decoded = decoder.decode_frame(&encoded).unwrap();
                assert!(!decoded.is_empty());
            }
            for _ in 0..3 {
                pipeline.process_frame(&sine(150, 320), false, 0.0);
            }
        }
        assert!(
            pipeline.leveler.gain_db > 2.0,
            "gain {:.2} dB",
            pipeline.leveler.gain_db
        );
    }

    #[test]
    fn full_scale_mic_asks_to_lower_the_os_volume() {
        let mut pipeline = AudioPipeline::new(false).expect("pipeline");
        let hot = vec![32_000_i16; 320];
        for i in 0..10 {
            let now = if i == 9 { 1.0 } else { 0.0 };
            pipeline.process_frame(&hot, false, now);
        }
        assert!(matches!(pipeline.take_clip_step(), Some(StepKind::Down(_))));
    }

    #[test]
    fn bytes_to_i16_round_trips() {
        let samples = [1_i16, -1, 32000, -32000];
        let bytes: Vec<u8> = samples.iter().flat_map(|v| v.to_le_bytes()).collect();
        assert_eq!(bytes_to_i16(&bytes), samples);
    }

    #[test]
    fn muted_mic_frame_keeps_leveler_warm_and_emits_silence_pcm() {
        let mut pipeline = AudioPipeline::new(false).expect("pipeline");
        let speech = sine(3000, 320);
        let (opus, tx_pcm) = pipeline
            .process_mic_frame(&speech, &speech, true, false, 0.0)
            .expect("encoded silence");
        assert!(!opus.is_empty());
        assert!(tx_pcm.iter().all(|&s| s == 0));
    }
}
