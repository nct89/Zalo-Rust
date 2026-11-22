//! WASAPI bridge: microphone → Opus, and Opus → speaker.
//!
//! Shares the exact `AudioBridge` surface with Linux and macOS so `zalo-gpui`
//! drives 1-1 and group calls uniformly across platforms.

use std::collections::{BTreeMap, VecDeque};
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::Instant;

use serde_json::Value;
use zcall_audio::{CardMic, CardSpeaker, FRAME_SAMPLES};
use zcall_core::audio::{OpusDecoder, OpusEncoder, OpusEncoderConfig, FRAME_SAMPLES as OPUS_SAMPLES};
use zcall_core::zrtc_helpers::{audio_encoder_options, playout_delay_ms};

/// An encoded Opus frame from the capture thread.
#[derive(Debug)]
pub struct AudioOut {
    /// Opus packet.
    pub opus: Vec<u8>,
    /// RFC 6464 level (0 = loudest, 127 = silence).
    pub level: u8,
}

struct Source {
    decoder: OpusDecoder,
    next_idx: Option<u32>,
    pending: BTreeMap<u32, Vec<u8>>,
    pcm: VecDeque<Vec<i16>>,
    primed: bool,
}

struct Mixer {
    sources: BTreeMap<Option<u32>, Source>,
    min_target: usize,
    max_target: usize,
}

impl Mixer {
    fn new(max_target: usize, min_target: usize) -> Self {
        Self {
            sources: BTreeMap::new(),
            min_target,
            max_target: max_target.max(min_target),
        }
    }

    fn set_limits(&mut self, max_target: usize, min_target: usize) {
        let min_target = min_target.max(2);
        self.min_target = min_target;
        self.max_target = max_target.max(min_target);
    }

    fn push(&mut self, idx: u32, payload: &[u8], source: Option<u32>) {
        let max_target = self.max_target;
        let slot = self.sources.entry(source).or_insert_with(|| Source {
            decoder: OpusDecoder::new().expect("opus decoder"),
            next_idx: None,
            pending: BTreeMap::new(),
            pcm: VecDeque::new(),
            primed: false,
        });
        if slot.next_idx.is_none() {
            slot.next_idx = Some(idx);
        }
        slot.pending.insert(idx, payload.to_vec());
        while slot.pending.len() > max_target.saturating_add(4) {
            if let Some(first) = slot.pending.keys().next().copied() {
                slot.pending.remove(&first);
                if slot.next_idx.is_some_and(|next| next < first) {
                    slot.next_idx = Some(first);
                }
            } else {
                break;
            }
        }
        decode_ready(slot);
        while slot.pcm.len() > max_target {
            slot.pcm.pop_front();
        }
    }

    fn mix(&mut self) -> Vec<i16> {
        let mut mixed = vec![0_i32; OPUS_SAMPLES];
        let min_target = self.min_target;
        let mut voices = 0_i32;
        for source in self.sources.values_mut() {
            if !source.primed {
                if source.pcm.len() < min_target {
                    continue;
                }
                source.primed = true;
            }
            let Some(frame) = source.pcm.pop_front() else {
                source.primed = false;
                continue;
            };
            voices += 1;
            for (dst, sample) in mixed.iter_mut().zip(frame.iter()) {
                *dst += i32::from(*sample);
            }
        }
        if voices == 0 {
            return vec![0_i16; OPUS_SAMPLES];
        }
        mixed
            .into_iter()
            .map(|sample| sample.clamp(i16::MIN as i32, i16::MAX as i32) as i16)
            .collect()
    }
}

fn decode_ready(slot: &mut Source) {
    let Some(mut next) = slot.next_idx else {
        return;
    };
    while let Some(packet) = slot.pending.remove(&next) {
        let mut pcm = vec![0_i16; OPUS_SAMPLES];
        if slot.decoder.decode(&packet, &mut pcm).is_ok() {
            slot.pcm.push_back(pcm);
        }
        next = next.wrapping_add(1);
        slot.next_idx = Some(next);
    }
}

/// Microphone and speaker bridge used by the call window.
pub struct AudioBridge {
    rx_out: Option<Receiver<AudioOut>>,
    cap_handle: Option<JoinHandle<()>>,
    play_handle: Option<JoinHandle<()>>,
    cap_stop: Arc<AtomicBool>,
    play_stop: Arc<AtomicBool>,
    speaker_off: Arc<AtomicBool>,
    speaker_wanted: Arc<(Mutex<bool>, Condvar)>,
    speaker_fault: Arc<AtomicBool>,
    muted: Arc<AtomicBool>,
    volume_pct: Arc<AtomicU8>,
    speaker_volume_pct: Arc<AtomicU8>,
    live_mic_level: Arc<AtomicU8>,
    mic_source: Option<String>,
    speaker_sink: Option<String>,
    encoder_cfg: OpusEncoderConfig,
    pending_encoder: Arc<Mutex<Option<OpusEncoderConfig>>>,
    mixer: Arc<Mutex<Mixer>>,
    jitter_min: usize,
    jitter_max: usize,
    defer_speaker: bool,
}

impl Default for AudioBridge {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for AudioBridge {
    fn drop(&mut self) {
        self.stop();
    }
}

impl AudioBridge {
    /// Builds an inactive bridge.
    #[must_use]
    pub fn new() -> Self {
        Self {
            rx_out: None,
            cap_handle: None,
            play_handle: None,
            cap_stop: Arc::new(AtomicBool::new(false)),
            play_stop: Arc::new(AtomicBool::new(false)),
            speaker_off: Arc::new(AtomicBool::new(false)),
            speaker_wanted: Arc::new((Mutex::new(false), Condvar::new())),
            speaker_fault: Arc::new(AtomicBool::new(false)),
            muted: Arc::new(AtomicBool::new(false)),
            volume_pct: Arc::new(AtomicU8::new(100)),
            speaker_volume_pct: Arc::new(AtomicU8::new(100)),
            live_mic_level: Arc::new(AtomicU8::new(127)),
            mic_source: None,
            speaker_sink: None,
            encoder_cfg: OpusEncoderConfig::default(),
            pending_encoder: Arc::new(Mutex::new(None)),
            mixer: Arc::new(Mutex::new(Mixer::new(10, 3))),
            jitter_min: 3,
            jitter_max: 10,
            defer_speaker: false,
        }
    }

    /// Keeps the speaker closed until the first decoded frame.
    pub fn defer_speaker(&mut self) {
        self.defer_speaker = true;
    }

    /// Applies the negotiated playout window. Values are milliseconds.
    pub fn set_jitter_ms(&mut self, min_ms: u32, max_ms: u32) {
        let min_frames = (min_ms as usize).saturating_add(19) / 20;
        let min_frames = min_frames.max(2);
        let max_frames = ((max_ms as usize).saturating_add(19) / 20).max(min_frames);
        self.jitter_min = min_frames;
        self.jitter_max = max_frames;
        if let Ok(mut mixer) = self.mixer.lock() {
            mixer.set_limits(max_frames, min_frames);
        }
    }

    /// Updates user-configured AEC and AI Noise Suppression toggles.
    ///
    /// Parity placeholder so `call.rs` shares the same `AudioBridge` interface
    /// across Linux, macOS, and Windows.
    pub fn set_audio_processing(&mut self, _aec: bool, _noise_suppression: bool) {}

    /// Applies jitter bounds and, for group calls, the Opus bitrate.
    pub fn apply_config(&mut self, group: bool, config: &Value) {
        let (min_ms, max_ms) = playout_delay_ms(config);
        self.set_jitter_ms(min_ms, max_ms);
        if !group {
            return;
        }
        let enc = audio_encoder_options(config);
        let next = OpusEncoderConfig {
            bitrate_bps: enc.bitrate_bps.unwrap_or(self.encoder_cfg.bitrate_bps),
            complexity: enc.complexity.unwrap_or(self.encoder_cfg.complexity),
            inband_fec: enc.inband_fec.unwrap_or(self.encoder_cfg.inband_fec),
            packet_loss_perc: if enc.inband_fec == Some(false) {
                0
            } else {
                self.encoder_cfg.packet_loss_perc
            },
        };
        if next != self.encoder_cfg {
            self.encoder_cfg = next;
            if let Ok(mut pending) = self.pending_encoder.lock() {
                *pending = Some(next);
            }
        }
    }

    /// Updates expected packet loss while FEC is on.
    pub fn set_packet_loss_perc(&mut self, loss_perc: i32) {
        if !self.encoder_cfg.inband_fec {
            return;
        }
        let clamped = loss_perc.clamp(5, 25);
        if (self.encoder_cfg.packet_loss_perc - clamped).abs() < 3 {
            return;
        }
        self.encoder_cfg.packet_loss_perc = clamped;
        if let Ok(mut pending) = self.pending_encoder.lock() {
            *pending = Some(self.encoder_cfg);
        }
    }

    /// Starts capture and playout.
    ///
    /// # Errors
    ///
    /// Returns an error when the microphone cannot be opened.
    pub fn start(
        &mut self,
        _mic_device: Option<&str>,
        mic_source: Option<&str>,
        speaker_sink: Option<&str>,
        _high_pass: bool,
    ) -> io::Result<()> {
        self.stop();
        if cfg!(test) {
            return Ok(());
        }
        self.mic_source = named(mic_source);
        self.speaker_sink = named(speaker_sink);
        self.cap_stop.store(false, Ordering::Release);
        self.play_stop.store(false, Ordering::Release);
        self.speaker_fault.store(false, Ordering::Release);
        if let Ok(mut mixer) = self.mixer.lock() {
            *mixer = Mixer::new(self.jitter_max, self.jitter_min);
        }
        let open_now = !self.defer_speaker;
        {
            let (lock, cv) = &*self.speaker_wanted;
            if let Ok(mut guard) = lock.lock() {
                *guard = open_now;
            }
            cv.notify_all();
        }
        self.spawn_playout();
        self.spawn_capture()?;
        Ok(())
    }

    /// Switches the microphone. Restarts capture when a call is active.
    ///
    /// # Errors
    ///
    /// Returns an error when the new device cannot be opened.
    pub fn set_source(&mut self, source: Option<&str>) -> io::Result<()> {
        self.mic_source = named(source);
        if self.cap_handle.is_some() {
            self.spawn_capture()?;
        }
        Ok(())
    }

    /// Switches the speaker. Restarts playout when a call is active.
    ///
    /// # Errors
    ///
    /// Returns an error when the device name cannot be applied.
    pub fn set_sink(&mut self, sink: Option<&str>) -> io::Result<()> {
        self.speaker_sink = named(sink);
        if self.play_handle.is_some() {
            self.spawn_playout();
        }
        Ok(())
    }

    /// Next encoded microphone frame, if the capture thread has one.
    pub fn next_audio_out(&mut self) -> Option<AudioOut> {
        self.rx_out.as_ref().and_then(|rx| rx.try_recv().ok())
    }

    /// Feeds one decrypted Opus packet into the jitter buffer.
    pub fn push_audio_in(&mut self, idx: u32, opus: &[u8], source: Option<u32>, _now: f64) {
        if let Ok(mut mixer) = self.mixer.lock() {
            mixer.push(idx, opus, source);
        }
        let (lock, cv) = &*self.speaker_wanted;
        if let Ok(mut guard) = lock.lock() {
            if !*guard {
                *guard = true;
                cv.notify_all();
            }
        }
    }

    /// Reports a sticky speaker fault from the playout thread.
    ///
    /// # Errors
    ///
    /// Returns an error when the last playout write failed.
    pub fn tick_playout(&mut self) -> io::Result<()> {
        if self.speaker_fault.swap(false, Ordering::AcqRel) {
            Err(io::Error::other("speaker write failed"))
        } else {
            Ok(())
        }
    }

    /// Mutes the speaker when `off` is true.
    pub fn set_speaker(&mut self, off: bool) {
        self.speaker_off.store(off, Ordering::Relaxed);
    }

    /// Speaker volume, 0..=100.
    pub fn set_speaker_volume(&mut self, percent: u8) {
        self.speaker_volume_pct
            .store(percent.min(100), Ordering::Relaxed);
    }

    /// Current speaker volume.
    #[must_use]
    pub fn speaker_volume_percent(&self) -> u8 {
        self.speaker_volume_pct.load(Ordering::Relaxed)
    }

    /// Latest microphone level.
    #[must_use]
    pub fn live_mic_level(&self) -> u8 {
        self.live_mic_level.load(Ordering::Relaxed)
    }

    /// Plays a short sine chime on the selected speaker.
    pub fn play_test_tone(&self) {
        if cfg!(test) {
            return;
        }
        let sink = self.speaker_sink.clone();
        let volume = self.speaker_volume_pct.load(Ordering::Relaxed);
        let _ = std::thread::Builder::new()
            .name("zcall-spk-test".into())
            .spawn(move || {
                let Ok(speaker) = CardSpeaker::open(sink.as_deref()) else {
                    return;
                };
                let mut frame = vec![0_i16; FRAME_SAMPLES];
                for step in 0..40_usize {
                    let gain = f32::from(volume) / 100.0 * 0.2;
                    for (index, sample) in frame.iter_mut().enumerate() {
                        let t = (step * FRAME_SAMPLES + index) as f32 / 16_000.0;
                        let wave = (2.0 * std::f32::consts::PI * 880.0 * t).sin();
                        *sample = (wave * gain * f32::from(i16::MAX)) as i16;
                    }
                    if speaker.write(&frame).is_err() {
                        break;
                    }
                }
            });
    }

    /// Mutes the microphone when `on` is true.
    pub fn set_mute(&mut self, on: bool) {
        self.muted.store(on, Ordering::Relaxed);
    }

    /// Digital microphone gain, 0..=100.
    pub fn set_mic_volume(&mut self, percent: u8) {
        self.volume_pct.store(percent.min(100), Ordering::Relaxed);
    }

    /// Stores the call-slider gain applied on the next captured frame.
    pub fn prepare_os_volume(&mut self, percent: u8) {
        self.volume_pct.store(percent.min(100), Ordering::Relaxed);
    }

    /// Current microphone gain.
    #[must_use]
    pub fn os_volume_percent(&self) -> u8 {
        self.volume_pct.load(Ordering::Relaxed)
    }

    /// Same as [`Self::prepare_os_volume`].
    pub fn set_os_mic_volume(&mut self, percent: u8) {
        self.prepare_os_volume(percent);
    }

    /// Whether the microphone is muted.
    #[must_use]
    pub fn is_muted(&self) -> bool {
        self.muted.load(Ordering::Relaxed)
    }

    /// Stops capture and playout threads.
    pub fn stop(&mut self) {
        self.cap_stop.store(true, Ordering::Release);
        self.play_stop.store(true, Ordering::Release);
        self.speaker_wanted.1.notify_all();
        if let Some(handle) = self.cap_handle.take() {
            let _ = handle.join();
        }
        if let Some(handle) = self.play_handle.take() {
            let _ = handle.join();
        }
        self.rx_out = None;
        self.cap_stop.store(false, Ordering::Release);
        self.play_stop.store(false, Ordering::Release);
    }

    fn spawn_capture(&mut self) -> io::Result<()> {
        self.cap_stop.store(true, Ordering::Release);
        if let Some(handle) = self.cap_handle.take() {
            let _ = handle.join();
        }
        self.cap_stop.store(false, Ordering::Release);
        let (tx, rx) = mpsc::channel();
        self.rx_out = Some(rx);
        let (ready_tx, ready_rx) = mpsc::channel();
        let source = self.mic_source.clone();
        let stop = Arc::clone(&self.cap_stop);
        let muted = Arc::clone(&self.muted);
        let volume = Arc::clone(&self.volume_pct);
        let level = Arc::clone(&self.live_mic_level);
        let pending = Arc::clone(&self.pending_encoder);
        let initial = self.encoder_cfg;
        let handle = std::thread::Builder::new()
            .name("zcall-mic".into())
            .spawn(move || {
                let mic = match CardMic::open(source.as_deref()) {
                    Ok(mic) => mic,
                    Err(err) => {
                        let _ = ready_tx.send(Err(io::Error::other(err.to_string())));
                        return;
                    }
                };
                let _ = ready_tx.send(Ok(()));
                capture_loop(mic, tx, stop, muted, volume, level, pending, initial);
            })
            .map_err(|err| io::Error::other(err.to_string()))?;
        match ready_rx.recv() {
            Ok(Ok(())) => {
                self.cap_handle = Some(handle);
                Ok(())
            }
            Ok(Err(err)) => {
                let _ = handle.join();
                Err(err)
            }
            Err(_) => {
                let _ = handle.join();
                Err(io::Error::other("microphone thread stopped before it opened"))
            }
        }
    }

    fn spawn_playout(&mut self) {
        self.play_stop.store(true, Ordering::Release);
        self.speaker_wanted.1.notify_all();
        if let Some(handle) = self.play_handle.take() {
            let _ = handle.join();
        }
        self.play_stop.store(false, Ordering::Release);
        let stop = Arc::clone(&self.play_stop);
        let wanted = Arc::clone(&self.speaker_wanted);
        let mixer = Arc::clone(&self.mixer);
        let speaker_off = Arc::clone(&self.speaker_off);
        let volume = Arc::clone(&self.speaker_volume_pct);
        let fault = Arc::clone(&self.speaker_fault);
        let sink = self.speaker_sink.clone();
        let handle = std::thread::Builder::new()
            .name("zcall-play".into())
            .spawn(move || {
                playout_loop(sink, stop, wanted, mixer, speaker_off, volume, fault);
            });
        self.play_handle = handle.ok();
    }
}

fn named(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|text| !text.is_empty() && *text != "Mặc định")
        .map(str::to_owned)
}

#[allow(clippy::too_many_arguments)]
fn capture_loop(
    mic: CardMic,
    tx: std::sync::mpsc::Sender<AudioOut>,
    stop: Arc<AtomicBool>,
    muted: Arc<AtomicBool>,
    volume: Arc<AtomicU8>,
    level_out: Arc<AtomicU8>,
    pending: Arc<Mutex<Option<OpusEncoderConfig>>>,
    initial: OpusEncoderConfig,
) {
    let Ok(mut encoder) = OpusEncoder::new() else {
        return;
    };
    let _ = encoder.apply_config(initial);
    while !stop.load(Ordering::Acquire) {
        if let Ok(mut slot) = pending.lock() {
            if let Some(cfg) = slot.take() {
                let _ = encoder.apply_config(cfg);
            }
        }
        let Ok(mut frame) = mic.read_frame() else {
            break;
        };
        if frame.len() != OPUS_SAMPLES {
            frame.resize(OPUS_SAMPLES, 0);
        }
        let gain = volume.load(Ordering::Relaxed);
        apply_gain(&mut frame, gain);
        if muted.load(Ordering::Relaxed) {
            frame.fill(0);
        }
        let level = audio_level(&frame);
        level_out.store(level, Ordering::Relaxed);
        let Ok(opus) = encoder.encode_frame(&frame) else {
            continue;
        };
        if tx.send(AudioOut { opus, level }).is_err() {
            break;
        }
    }
}

fn playout_loop(
    sink: Option<String>,
    stop: Arc<AtomicBool>,
    wanted: Arc<(Mutex<bool>, Condvar)>,
    mixer: Arc<Mutex<Mixer>>,
    speaker_off: Arc<AtomicBool>,
    volume: Arc<AtomicU8>,
    fault: Arc<AtomicBool>,
) {
    {
        let (lock, cv) = &*wanted;
        let guard = lock.lock().unwrap_or_else(|err| err.into_inner());
        if !*guard {
            let _ = cv.wait_timeout(guard, std::time::Duration::from_secs(30));
        }
    }
    if stop.load(Ordering::Acquire) {
        return;
    }
    let Ok(speaker) = CardSpeaker::open(sink.as_deref()) else {
        fault.store(true, Ordering::Release);
        return;
    };
    let _ = speaker.prime_silence(4);
    while !stop.load(Ordering::Acquire) {
        let mut frame = mixer
            .lock()
            .map(|mut mixer| mixer.mix())
            .unwrap_or_else(|_| vec![0_i16; OPUS_SAMPLES]);
        if speaker_off.load(Ordering::Relaxed) {
            frame.fill(0);
        } else {
            apply_gain(&mut frame, volume.load(Ordering::Relaxed));
        }
        if speaker.write(&frame).is_err() {
            fault.store(true, Ordering::Release);
            break;
        }
    }
}

fn apply_gain(pcm: &mut [i16], percent: u8) {
    if percent >= 100 {
        return;
    }
    let gain = f32::from(percent) / 100.0;
    for sample in pcm {
        *sample = (f32::from(*sample) * gain) as i16;
    }
}

/// RFC 6464 audio level.
#[must_use]
pub fn audio_level(pcm: &[i16]) -> u8 {
    if pcm.is_empty() {
        return 127;
    }
    let sum: f64 = pcm
        .iter()
        .map(|sample| {
            let value = f64::from(*sample);
            value * value
        })
        .sum();
    let rms = (sum / pcm.len() as f64).sqrt();
    if rms < 1.0 {
        return 127;
    }
    let dbov = -20.0 * (rms / 32768.0).log10();
    dbov.round().clamp(0.0, 127.0) as u8
}

/// Seconds since the first call in this process.
#[must_use]
pub fn monotonic_secs() -> f64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_is_level_127_and_gain_scales() {
        assert_eq!(audio_level(&[0, 0, 0, 0]), 127);
        let mut pcm = vec![10_000_i16, -10_000];
        apply_gain(&mut pcm, 50);
        assert_eq!(pcm, vec![5_000, -5_000]);
    }

    #[test]
    fn start_in_tests_does_not_open_wasapi() {
        let mut bridge = AudioBridge::new();
        bridge.start(None, None, None, false).expect("start");
        assert!(bridge.next_audio_out().is_none());
        assert!(bridge.tick_playout().is_ok());
        bridge.push_audio_in(1, &[0], None, monotonic_secs());
        bridge.set_mute(true);
        assert!(bridge.is_muted());
        bridge.set_os_mic_volume(40);
        assert_eq!(bridge.os_volume_percent(), 40);
        bridge.stop();
    }
}
