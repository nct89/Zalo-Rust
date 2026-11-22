//! Bridges the Rust audio pipeline (`zcall-audio`) to the media backend,
//! replacing the original's `spawn(audio-io.py)` subprocess.
//!
//! A background thread reads the microphone, processes each frame through
//! ClipGuard + MicLeveler + Opus, and sends the encoded frame through a
//! channel. The main loop receives encoded frames and forwards them to
//! `ZrtcMediaBackend.send_audio()`.
//!
//! On the playout side, decrypted Opus from the peer goes into a `JitterBuffer`.
//! PCM is written through `libpulse-simple`, so the sound card clocks the loop
//! the way `audio-io.py` `PulseOut` does. `pacat` is only the fallback.

use serde_json::Value;
use std::collections::VecDeque;
use std::io::{self, Read};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use zcall_audio::apm::InProcessApm;
use zcall_audio::capture::{
    live_mic_command, live_speaker_command, CardMic, CardSpeaker, MicCapture, Speaker,
    CARD_LATENCY_MS, CARD_MIC_LATENCY_MS, FRAME_BYTES,
};
use zcall_audio::mixer::Mixer;
use zcall_audio::pipeline::AudioPipeline;
use zcall_audio::processing::AudioProcessing;
use zcall_audio::wsola::FRAME_SAMPLES;
use zcall_core::audio::OpusEncoderConfig;
use zcall_core::clip_guard::{db_to_raw, percent_to_raw, ClipGuard, StepKind};
use zcall_core::zrtc_helpers::{
    audio_encoder_options, audio_processing_options, playout_delay_ms, AudioProcessingOptions,
};

const PLAYOUT_FRAME: Duration = Duration::from_millis(20);
const PLAYOUT_RESYNC: Duration = Duration::from_millis(100);
/// Silence written before the microphone, matching `prime_playback` (5 × 20 ms).
const CARD_PRIME_FRAMES: usize = 5;
const PLAYOUT_PRIME_FRAMES: usize = 4;

/// An encoded Opus frame from the capture thread, ready to send.
#[derive(Debug)]
pub struct AudioOut {
    /// Opus-encoded audio.
    pub opus: Vec<u8>,
    /// RFC 6464 audio level (0..127, lower = louder).
    pub level: u8,
}

/// Shared playout coordination between [`AudioBridge`] and `zcall-play`.
struct PlayoutShared {
    mixer: Arc<Mutex<Mixer>>,
    apm: Arc<InProcessApm>,
    use_in_process_apm: Arc<AtomicBool>,
    speaker_off: Arc<AtomicBool>,
    speaker_volume_pct: Arc<AtomicU8>,
    far_heard: Arc<AtomicBool>,
    wanted: Arc<AtomicBool>,
    fault: Arc<AtomicBool>,
    sink: Arc<Mutex<Option<String>>>,
    sink_epoch: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
}

/// The audio bridge: mic → DSP → Opus → channel, and jitter → speaker.
pub struct AudioBridge {
    /// Receives encoded Opus frames from the capture thread.
    rx_out: Option<Receiver<AudioOut>>,
    /// The capture thread handle.
    cap_handle: Option<JoinHandle<()>>,
    /// The dedicated 20 ms playout thread handle.
    play_handle: Option<JoinHandle<()>>,
    /// Playout mixer shared with `zcall-play`.
    mixer: Arc<Mutex<Mixer>>,
    /// Fallback synchronous speaker (only used when `tick_playout` is driven
    /// manually without calling `start`).
    speaker: Option<Speaker>,
    /// Set to stop the capture thread.
    stop_flag: Arc<AtomicBool>,
    /// Set to stop `zcall-play`.
    play_stop: Arc<AtomicBool>,
    /// Speaker off state.
    speaker_off: Arc<AtomicBool>,
    /// Whether `zcall-play` should keep the `pacat` speaker stream open.
    speaker_wanted: Arc<AtomicBool>,
    /// Sticky fault from `zcall-play`, consumed once by `tick_playout`.
    speaker_fault: Arc<AtomicBool>,
    /// Effective PipeWire/Pulse sink name seen by `zcall-play`.
    effective_sink: Arc<Mutex<Option<String>>>,
    /// Incremented whenever `effective_sink` changes.
    sink_epoch: Arc<AtomicU64>,
    /// Microphone mute. The capture thread reads this.
    muted: Arc<AtomicBool>,
    /// Microphone gain, 0..=100. The capture thread reads this.
    ///
    /// Digital only. The call slider does not use it: scaling here and in
    /// Pulse would apply the cut twice. It stays at 100 unless a caller asks
    /// for digital attenuation.
    volume_pct: Arc<AtomicU8>,
    /// Pulse source level, 0..=100, written with `pactl`. 100 is 0 dB.
    os_volume_pct: u8,
    /// The user moved the slider. The capture thread tells ClipGuard to stop.
    clip_manual: Arc<AtomicBool>,
    /// Latest `pactl` request. An older write bails out.
    vol_gen: Arc<AtomicU64>,
    /// ALSA device, when not using Pulse. Empty uses Pulse.
    mic_device: Option<String>,
    /// PulseAudio source name. Empty is the system default.
    mic_source: Option<String>,
    /// PulseAudio sink name. Empty is the system default.
    speaker_sink: Option<String>,
    /// Legacy external PipeWire WebRTC echo canceller (opt-in via `ZCALL_LEGACY_PW_AEC=1`).
    processing: Option<AudioProcessing>,
    /// In-process WebRTC APM (`sonora`: AEC3, leveled NS, AGC2 + limiter, HPF).
    apm: Arc<InProcessApm>,
    /// True when in-process APM owns DSP (default); false when legacy external PW AEC is forced.
    use_in_process_apm: Arc<AtomicBool>,
    /// Negotiated audio processing toggles.
    proc_opts: AudioProcessingOptions,
    /// User preference overrides from Call Settings / Device Status (`callAec`, `callNoiseSuppression`).
    user_aec: Option<bool>,
    user_ns: Option<bool>,
    /// Active Opus encoder settings.
    encoder_cfg: OpusEncoderConfig,
    /// Pending Opus encoder settings for `zcall-mic` to apply live.
    pending_encoder: Arc<Mutex<Option<OpusEncoderConfig>>>,
    /// High-pass filter, kept so a device change restarts the same pipeline.
    high_pass: bool,
    /// Jitter floor in 20 ms frames (`minAudioDelayMs`, default 100 ms).
    jitter_min: usize,
    /// Jitter ceiling in 20 ms frames (`maxAudioDelayMs`, default 200 ms).
    jitter_max: usize,
    /// The microphone process. Killing it unblocks the capture thread.
    mic: Option<MicCapture>,
    /// The far end was just played. The capture thread holds MicLeveler gain.
    far_heard: Arc<AtomicBool>,
    /// Group calls open the speaker on the first decoded frame when raw Pulse
    /// is used; when AEC `Processing` is active the speaker starts primed so
    /// the canceller has its reference stream immediately.
    defer_speaker: bool,
    /// Bluetooth card `(device_id, prev_profile_index)` switched from A2DP to
    /// HFP/mSBC for the call, restored automatically on hangup or mic switch.
    bt_restore: Option<(u32, u32)>,
    /// True while capturing from a Bluetooth HFP headset (hardware on-chip
    /// AEC/NS active, so software AEC3 is bypassed unless `ZCALL_AEC=1`).
    bt_capture_active: Arc<AtomicBool>,
    /// Speaker output volume, 0..=100 (default 100).
    speaker_volume_pct: Arc<AtomicU8>,
    /// Latest live microphone level (RFC 6464: 0 loudest .. 127 silence).
    live_mic_level: Arc<AtomicU8>,
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
    pub fn new() -> Self {
        let jitter_min = 3;
        let jitter_max = 10;
        let proc_opts = AudioProcessingOptions::default();
        let legacy_pw = legacy_pipewire_aec_requested();
        Self {
            rx_out: None,
            cap_handle: None,
            play_handle: None,
            mixer: Arc::new(Mutex::new(Mixer::new(jitter_max, Some(jitter_min)))),
            speaker: None,
            stop_flag: Arc::new(AtomicBool::new(false)),
            play_stop: Arc::new(AtomicBool::new(false)),
            speaker_off: Arc::new(AtomicBool::new(false)),
            speaker_wanted: Arc::new(AtomicBool::new(false)),
            speaker_fault: Arc::new(AtomicBool::new(false)),
            effective_sink: Arc::new(Mutex::new(None)),
            sink_epoch: Arc::new(AtomicU64::new(0)),
            muted: Arc::new(AtomicBool::new(false)),
            volume_pct: Arc::new(AtomicU8::new(100)),
            os_volume_pct: 100,
            clip_manual: Arc::new(AtomicBool::new(false)),
            vol_gen: Arc::new(AtomicU64::new(0)),
            mic_device: None,
            mic_source: None,
            speaker_sink: None,
            processing: None,
            apm: Arc::new(InProcessApm::new(proc_opts)),
            use_in_process_apm: Arc::new(AtomicBool::new(!legacy_pw)),
            proc_opts,
            user_aec: None,
            user_ns: None,
            encoder_cfg: OpusEncoderConfig::default(),
            pending_encoder: Arc::new(Mutex::new(None)),
            high_pass: false,
            jitter_min,
            jitter_max,
            mic: None,
            far_heard: Arc::new(AtomicBool::new(false)),
            defer_speaker: false,
            bt_restore: None,
            bt_capture_active: Arc::new(AtomicBool::new(false)),
            speaker_volume_pct: Arc::new(AtomicU8::new(100)),
            live_mic_level: Arc::new(AtomicU8::new(127)),
        }
    }

    /// Keeps the speaker closed until the first decoded frame arrives (when AEC
    /// subprocess is inactive).
    pub fn defer_speaker(&mut self) {
        self.defer_speaker = true;
    }

    /// Applies the negotiated playout window. `min_ms` / `max_ms` come from
    /// `playout_delay_ms` (Electron `callAudioArgs`).
    pub fn set_jitter_ms(&mut self, min_ms: u32, max_ms: u32) {
        let min_frames = (min_ms as usize).saturating_add(19) / 20;
        let min_frames = min_frames.max(2);
        let max_frames = ((max_ms as usize).saturating_add(19) / 20).max(min_frames);
        self.jitter_min = min_frames;
        self.jitter_max = max_frames;
        let mut mixer = self.mixer.lock().unwrap_or_else(|e| e.into_inner());
        mixer.set_limits(max_frames, min_frames);
    }

    /// Updates user-configured AEC and AI Noise Suppression toggles live
    /// without reopening microphone or speaker streams.
    pub fn set_audio_processing(&mut self, aec: bool, noise_suppression: bool) {
        self.user_aec = Some(aec);
        self.user_ns = Some(noise_suppression);
        self.proc_opts.aec = aec && zcall_core::zrtc_helpers::echo_cancellation_requested();
        self.proc_opts.noise_suppression =
            noise_suppression && zcall_core::zrtc_helpers::noise_suppression_requested();
        if self.use_in_process_apm.load(Ordering::Acquire) {
            self.apm.apply_options(self.proc_opts);
        }
    }

    /// Applies negotiated `zrtcConfig` parameters: jitter bounds, group Opus
    /// bitrate/complexity/FEC (`groupAudioArgs`), and PipeWire AEC toggles.
    pub fn apply_config(&mut self, group: bool, config: &Value) {
        let (min_ms, max_ms) = playout_delay_ms(config);
        self.set_jitter_ms(min_ms, max_ms);

        let mut proc_opts = audio_processing_options(config);
        if let Some(aec) = self.user_aec {
            proc_opts.aec = aec && zcall_core::zrtc_helpers::echo_cancellation_requested();
        }
        if let Some(ns) = self.user_ns {
            proc_opts.noise_suppression =
                ns && zcall_core::zrtc_helpers::noise_suppression_requested();
        }
        let proc_changed = proc_opts != self.proc_opts;
        self.proc_opts = proc_opts;
        if self.use_in_process_apm.load(Ordering::Acquire) {
            // Reconfigure WebRTC APM live in-place: zero mic/speaker teardown
            // or mid-call audio dropout when `zrtcConfig` arrives.
            self.apm.apply_options(proc_opts);
        }

        let enc_opts = audio_encoder_options(config);
        // Official 1-1 and group configs set audioHighPassFilter false.
        let mut hp_changed = false;
        if let Some(hp) = enc_opts.high_pass {
            if hp != self.high_pass {
                self.high_pass = hp;
                hp_changed = true;
            }
        }

        if group {
            let next_enc = OpusEncoderConfig {
                bitrate_bps: enc_opts.bitrate_bps.unwrap_or(self.encoder_cfg.bitrate_bps),
                complexity: enc_opts.complexity.unwrap_or(self.encoder_cfg.complexity),
                inband_fec: enc_opts.inband_fec.unwrap_or(self.encoder_cfg.inband_fec),
                packet_loss_perc: if enc_opts.inband_fec == Some(false) {
                    0
                } else {
                    self.encoder_cfg.packet_loss_perc
                },
            };
            if next_enc != self.encoder_cfg {
                self.encoder_cfg = next_enc;
                *self
                    .pending_encoder
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = Some(next_enc);
            }
        }

        if !self.use_in_process_apm.load(Ordering::Acquire) {
            if proc_changed && (self.cap_handle.is_some() || self.play_handle.is_some()) {
                self.restart_processing();
            }
            if (proc_changed || hp_changed) && self.cap_handle.is_some() {
                let _ = self.open_capture();
            }
        }
    }

    /// Updates expected uplink packet loss percentage (5..=25 %) while FEC is
    /// enabled so Opus adjusts redundant LBRR overhead dynamically.
    pub fn set_packet_loss_perc(&mut self, loss_perc: i32) {
        if !self.encoder_cfg.inband_fec {
            return;
        }
        let clamped = loss_perc.clamp(5, 25);
        if (self.encoder_cfg.packet_loss_perc - clamped).abs() < 3 {
            return;
        }
        self.encoder_cfg.packet_loss_perc = clamped;
        *self
            .pending_encoder
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(self.encoder_cfg);
    }

    /// Starts capture and playout. Returns an error when a subprocess cannot
    /// be spawned.
    pub fn start(
        &mut self,
        mic_device: Option<&str>,
        mic_source: Option<&str>,
        speaker_sink: Option<&str>,
        high_pass: bool,
    ) -> io::Result<()> {
        self.stop();
        self.mic_device = named(mic_device);
        self.mic_source = named(mic_source);
        self.speaker_sink = named(speaker_sink);
        self.high_pass = high_pass;
        // Before parec. A source left at the clip-guard floor (−24 dB) stays
        // quiet for the whole call if the guard snapshots that level first.
        self.write_os_volume(true);
        self.far_heard.store(false, Ordering::Relaxed);
        self.speaker_fault.store(false, Ordering::Relaxed);
        {
            let mut mixer = self.mixer.lock().unwrap_or_else(|e| e.into_inner());
            *mixer = Mixer::new(self.jitter_max, Some(self.jitter_min));
        }

        let legacy_pw = legacy_pipewire_aec_requested();
        self.use_in_process_apm.store(!legacy_pw, Ordering::Release);
        self.apm.reset(self.proc_opts);
        if self.mic_device.is_none() && std::env::var_os("ZCALL_NO_PULSE").is_none() {
            let _ = std::thread::Builder::new()
                .name("zcall-pw-rt".into())
                .spawn(zcall_audio::processing::ensure_pipewire_realtime);
            if legacy_pw {
                self.processing = AudioProcessing::start(
                    self.mic_source.as_deref(),
                    self.speaker_sink.as_deref(),
                    self.proc_opts,
                );
            }
        }
        self.sync_effective_sink();
        // Start and prime speaker before opening mic so AEC has an initialized
        // playback reference stream immediately.
        let open_now =
            self.processing.is_some() || self.use_in_process_apm.load(Ordering::Acquire) || !self.defer_speaker;
        self.speaker_wanted.store(open_now, Ordering::Release);
        self.start_playout_thread(open_now)?;
        if let Err(err) = self.open_capture() {
            self.stop();
            return Err(err);
        }
        Ok(())
    }

    fn effective_mic_source(&self) -> Option<String> {
        self.processing
            .as_ref()
            .map(|p| p.source_name().to_owned())
            .or_else(|| self.mic_source.clone())
    }

    fn effective_speaker_sink(&self) -> Option<String> {
        self.processing
            .as_ref()
            .map(|p| p.sink_name().to_owned())
            .or_else(|| self.speaker_sink.clone())
    }

    fn sync_effective_sink(&self) {
        let next = self.effective_speaker_sink();
        *self
            .effective_sink
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = next;
        self.sink_epoch.fetch_add(1, Ordering::AcqRel);
        if let Some(handle) = &self.play_handle {
            handle.thread().unpark();
        }
    }

    fn restart_processing(&mut self) {
        self.processing = None;
        let legacy_pw = legacy_pipewire_aec_requested();
        self.use_in_process_apm.store(!legacy_pw, Ordering::Release);
        if self.mic_device.is_none() && std::env::var_os("ZCALL_NO_PULSE").is_none() {
            let _ = std::thread::Builder::new()
                .name("zcall-pw-rt".into())
                .spawn(zcall_audio::processing::ensure_pipewire_realtime);
            if legacy_pw {
                self.processing = AudioProcessing::start(
                    self.mic_source.as_deref(),
                    self.speaker_sink.as_deref(),
                    self.proc_opts,
                );
            }
        }
        if self.processing.is_some() || !legacy_pw {
            self.speaker_wanted.store(true, Ordering::Release);
        }
        self.sync_effective_sink();
    }

    fn start_playout_thread(&mut self, wait_ready: bool) -> io::Result<()> {
        self.play_stop.store(false, Ordering::Release);
        let shared = PlayoutShared {
            mixer: Arc::clone(&self.mixer),
            apm: Arc::clone(&self.apm),
            use_in_process_apm: Arc::clone(&self.use_in_process_apm),
            speaker_off: Arc::clone(&self.speaker_off),
            speaker_volume_pct: Arc::clone(&self.speaker_volume_pct),
            far_heard: Arc::clone(&self.far_heard),
            wanted: Arc::clone(&self.speaker_wanted),
            fault: Arc::clone(&self.speaker_fault),
            sink: Arc::clone(&self.effective_sink),
            sink_epoch: Arc::clone(&self.sink_epoch),
            stop: Arc::clone(&self.play_stop),
        };
        let (ready_tx, ready_rx) = mpsc::sync_channel::<io::Result<()>>(1);
        let handle = std::thread::Builder::new()
            .name("zcall-play".into())
            .spawn(move || run_playout_loop(shared, ready_tx))?;
        self.play_handle = Some(handle);
        if wait_ready {
            match ready_rx.recv() {
                Ok(Ok(())) => {}
                Ok(Err(err)) => {
                    self.stop_playout_thread();
                    return Err(err);
                }
                Err(_) => {
                    self.stop_playout_thread();
                    return Err(io::Error::other("playout thread exited before ready"));
                }
            }
        }
        Ok(())
    }

    fn stop_playout_thread(&mut self) {
        self.play_stop.store(true, Ordering::Release);
        if let Some(handle) = self.play_handle.take() {
            handle.thread().unpark();
            let _ = handle.join();
        }
    }

    /// Switches the microphone. An empty name is the system default.
    /// Applies immediately when capture is already running.
    pub fn set_source(&mut self, source: Option<&str>) -> io::Result<()> {
        self.mic_source = named(source);
        if self.cap_handle.is_some() {
            let legacy_pw = self.processing.is_some() || legacy_pipewire_aec_requested();
            if legacy_pw {
                self.restart_processing();
            } else if self.use_in_process_apm.load(Ordering::Acquire) {
                self.apm.reset(self.proc_opts);
            }
            self.open_capture()?;
            self.write_os_volume(true);
            Ok(())
        } else {
            Ok(())
        }
    }

    /// Switches the speaker. An empty name is the system default.
    /// Applies immediately when playout is already running.
    pub fn set_sink(&mut self, sink: Option<&str>) -> io::Result<()> {
        self.speaker_sink = named(sink);
        if self.play_handle.is_some() {
            if self.use_in_process_apm.load(Ordering::Acquire) {
                self.apm.reset(self.proc_opts);
            }
            self.restart_processing();
            if self.cap_handle.is_some() && self.processing.is_some() {
                self.open_capture()?;
            }
            Ok(())
        } else if self.speaker.is_some() {
            self.open_speaker()
        } else {
            Ok(())
        }
    }

    fn open_speaker(&mut self) -> io::Result<()> {
        let mut speaker = Speaker::spawn(&live_speaker_command(self.speaker_sink.as_deref()))?;
        let _ = speaker.prime_silence(PLAYOUT_PRIME_FRAMES);
        if let Some(mut old) = self.speaker.replace(speaker) {
            old.close();
        }
        Ok(())
    }

    /// Stops the current microphone, if any, and starts the saved source.
    fn open_capture(&mut self) -> io::Result<()> {
        self.stop_flag.store(true, Ordering::SeqCst);
        if let Some(mut mic) = self.mic.take() {
            mic.close();
        }
        if let Some(handle) = self.cap_handle.take() {
            let _ = handle.join();
        }
        self.stop_flag.store(false, Ordering::SeqCst);

        let pulse = std::env::var("ZCALL_NO_PULSE").is_err();
        let cap_source = self.effective_mic_source();
        // If capture targets a Bluetooth headset currently in A2DP (playback-only,
        // where `bluez_input.*` yields all-zero silence), switch its PipeWire card
        // to HFP/mSBC (`headset-head-unit`) and restore A2DP when done.
        if self.mic_device.is_none() && pulse {
            let profile_changed = self.ensure_bt_hfp_for_source(cap_source.as_deref());
            if profile_changed {
                self.sync_effective_sink();
            }
        }
        // Prefer in-process `CardMic` (`libpulse-simple`) on the sound-card
        // clock so capture latency is measured directly for WebRTC AEC3 and
        // no external `parec` subprocess/pipe is spawned. Fall back to
        // `MicCapture` (`parec` / `pw-record` / `arecord`) if unavailable.
        let mut card_mic: Option<CardMic> = None;
        if self.mic_device.is_none() && pulse {
            match CardMic::open(cap_source.as_deref()).or_else(|err| {
                if cap_source.is_some() {
                    eprintln!("zcall-mic: explicit source failed ({err}); retrying default");
                    CardMic::open(None)
                } else {
                    Err(err)
                }
            }) {
                Ok(cm) => card_mic = Some(cm),
                Err(err) => {
                    static NOTED: AtomicBool = AtomicBool::new(false);
                    if !NOTED.swap(true, Ordering::Relaxed) {
                        eprintln!("zcall-mic: libpulse-simple record unavailable ({err}); parec");
                    }
                }
            }
        }
        let mut proc_mic: Option<MicCapture> = None;
        let mut proc_stdout: Option<std::process::ChildStdout> = None;
        if card_mic.is_none() {
            let mic_cmd = live_mic_command(self.mic_device.as_deref(), cap_source.as_deref(), pulse);
            let mut mic = MicCapture::spawn(&mic_cmd)?;
            let stdout = mic
                .take_stdout()
                .ok_or_else(|| io::Error::other("microphone has no stdout"))?;
            proc_mic = Some(mic);
            proc_stdout = Some(stdout);
        }

        let stop = Arc::clone(&self.stop_flag);
        let muted = Arc::clone(&self.muted);
        let volume = Arc::clone(&self.volume_pct);
        let far = Arc::clone(&self.far_heard);
        let apm = Arc::clone(&self.apm);
        let use_in_process_apm = Arc::clone(&self.use_in_process_apm);
        let bt_capture = Arc::clone(&self.bt_capture_active);
        let live_mic_level = Arc::clone(&self.live_mic_level);
        let force_aec = std::env::var("ZCALL_AEC").ok().as_deref() == Some("1");
        let high_pass = self.high_pass;
        let enc_cfg = self.encoder_cfg;
        let pending_enc = Arc::clone(&self.pending_encoder);
        let source = volume_source(self.mic_source.as_deref());
        let clip_manual = Arc::clone(&self.clip_manual);
        let (tx_out, rx_out) = mpsc::channel::<AudioOut>();
        // Opus complexity 10 + APM run off the reader thread so capture never
        // stalls on the audio daemon.
        let pcm_queue = Arc::new(Mutex::new(VecDeque::<(Vec<i16>, u32)>::new()));
        let pcm_ready = Arc::new(Condvar::new());
        let reader_stop = Arc::clone(&stop);
        let reader_queue = Arc::clone(&pcm_queue);
        let reader_ready = Arc::clone(&pcm_ready);
        let reader_done = Arc::new(AtomicBool::new(false));
        let reader_finished = Arc::clone(&reader_done);
        let reader = std::thread::Builder::new()
            .name("zcall-mic-read".into())
            .spawn(move || {
                if let Some(cm) = card_mic {
                    let mut latency_tick = 0_u32;
                    let mut cap_lat = CARD_MIC_LATENCY_MS;
                    while !reader_stop.load(Ordering::SeqCst) {
                        let Ok(frame) = cm.read_frame() else {
                            break;
                        };
                        if latency_tick % 25 == 0 {
                            cap_lat = cm.latency_ms();
                        }
                        latency_tick = latency_tick.wrapping_add(1);
                        let mut q = reader_queue.lock().unwrap_or_else(|err| err.into_inner());
                        if q.len() == 5 {
                            q.pop_front();
                        }
                        q.push_back((frame, cap_lat));
                        reader_ready.notify_one();
                    }
                } else if let Some(mut stdout) = proc_stdout {
                    while !reader_stop.load(Ordering::SeqCst) {
                        let Some(frame) = read_pcm_frame(&mut stdout) else {
                            break;
                        };
                        let mut q = reader_queue.lock().unwrap_or_else(|err| err.into_inner());
                        // Keep the newest 100 ms. Dropping the oldest frame loses
                        // 20 ms; blocking here lets PipeWire tear the byte stream.
                        if q.len() == 5 {
                            q.pop_front();
                        }
                        q.push_back((zcall_audio::bytes_to_i16(&frame), CARD_MIC_LATENCY_MS));
                        reader_ready.notify_one();
                    }
                }
                reader_finished.store(true, Ordering::SeqCst);
                reader_ready.notify_one();
            })?;
        let handle = std::thread::Builder::new()
            .name("zcall-mic".into())
            .spawn(move || {
                let mut pipeline = match AudioPipeline::with_config(high_pass, enc_cfg) {
                    Ok(p) => p,
                    Err(_) => {
                        let _ = reader.join();
                        return;
                    }
                };
                let clip_busy = Arc::new(AtomicBool::new(false));
                let mut manual_on = false;
                'run: loop {
                    let (raw, cap_lat) = {
                        let mut q = pcm_queue.lock().unwrap_or_else(|err| err.into_inner());
                        loop {
                            if let Some(item) = q.pop_front() {
                                break item;
                            }
                            if stop.load(Ordering::SeqCst) || reader_done.load(Ordering::SeqCst) {
                                break 'run;
                            }
                            let (next, wait) = pcm_ready
                                .wait_timeout(q, Duration::from_millis(200))
                                .unwrap_or_else(|err| err.into_inner());
                            q = next;
                            if wait.timed_out()
                                && (stop.load(Ordering::SeqCst)
                                    || reader_done.load(Ordering::SeqCst))
                            {
                                break 'run;
                            }
                        }
                    };
                    if !manual_on && clip_manual.load(Ordering::Relaxed) {
                        pipeline
                            .clip_guard
                            .lock()
                            .unwrap_or_else(|err| err.into_inner())
                            .set_manual(true);
                        manual_on = true;
                    }
                    if let Some(next_enc) =
                        pending_enc.lock().unwrap_or_else(|e| e.into_inner()).take()
                    {
                        let _ = pipeline.apply_encoder_config(next_enc);
                    }
                    let is_muted = muted.load(Ordering::Relaxed);
                    let vol = volume.load(Ordering::Relaxed);
                    let scaled = if vol >= 100 {
                        raw.clone()
                    } else {
                        apply_mic(raw.clone(), false, vol)
                    };
                    let now = monotonic_secs();
                    let heard = far.load(Ordering::Relaxed);
                    let allow_aec = !bt_capture.load(Ordering::Acquire) || force_aec;
                    let run_apm = use_in_process_apm.load(Ordering::Acquire) && apm.is_enabled();
                    let encoded = if run_apm {
                        // Keep WebRTC AEC3 converged even while muted, matching
                        // Windows `ZaloCall.exe`, and keep RNNoise active even
                        // when Bluetooth HFP bypasses software AEC3.
                        let cleaned = apm.process_capture_with_aec(&scaled, cap_lat, allow_aec);
                        if apm.has_gain_control() {
                            pipeline.process_apm_frame(&cleaned, is_muted)
                        } else {
                            pipeline.process_mic_frame(&raw, &cleaned, is_muted, heard, now)
                        }
                    } else {
                        pipeline.process_mic_frame(&raw, &scaled, is_muted, heard, now)
                    };
                    if let Some((opus, tx_pcm)) = encoded {
                        let level = audio_level(&tx_pcm);
                        live_mic_level.store(level, Ordering::Relaxed);
                        if tx_out.send(AudioOut { opus, level }).is_err() {
                            break 'run;
                        }
                    }
                    if std::env::var("ZCALL_CLIP_GUARD").ok().as_deref() == Some("1") {
                        if let Some(kind) = pipeline.take_clip_step() {
                            request_volume_step(&pipeline.clip_guard(), &clip_busy, &source, kind);
                        }
                    }
                }
                let _ = reader.join();
            })?;
        self.mic = proc_mic;
        self.cap_handle = Some(handle);
        self.rx_out = Some(rx_out);
        Ok(())
    }

    /// Returns the next encoded Opus frame from the microphone, if any.
    pub fn next_audio_out(&mut self) -> Option<AudioOut> {
        self.rx_out.as_ref().and_then(|rx| rx.try_recv().ok())
    }

    /// Feeds a decrypted Opus frame into one source.
    ///
    /// `source` is the sender SSRC. 1-1 still passes it: one SSRC is one
    /// source, so the mix is that voice alone.
    pub fn push_audio_in(&mut self, idx: u32, opus: &[u8], source: Option<u32>, now: f64) {
        {
            let mut mixer = self.mixer.lock().unwrap_or_else(|e| e.into_inner());
            mixer.push(idx, opus, source, now);
        }
        if self.defer_speaker && !self.speaker_wanted.swap(true, Ordering::AcqRel) {
            if let Some(handle) = &self.play_handle {
                handle.thread().unpark();
            } else if self.speaker.is_none() {
                let _ = self.open_speaker();
            }
        }
    }

    /// Reports speaker faults when `zcall-play` is running, or mixes/writes one
    /// frame directly if driven manually in isolation.
    pub fn tick_playout(&mut self) -> io::Result<()> {
        if self.play_handle.is_some() {
            return if self.speaker_fault.swap(false, Ordering::AcqRel) {
                Err(io::Error::other("speaker write failed"))
            } else {
                Ok(())
            };
        }
        let Some(speaker) = self.speaker.as_mut() else {
            return Ok(());
        };
        let now = monotonic_secs();
        let speaker_off = self.speaker_off.load(Ordering::Relaxed);
        let play = {
            let mut mixer = self.mixer.lock().unwrap_or_else(|e| e.into_inner());
            let mixed = mixer.mix(now);
            let play = if speaker_off {
                vec![0_i16; FRAME_SAMPLES]
            } else {
                mixed
            };
            mixer.note_played(&play, now);
            self.far_heard
                .store(mixer.far_active(now), Ordering::Relaxed);
            play
        };
        speaker.write(&play)?;
        Ok(())
    }

    /// Enables or disables the speaker.
    pub fn set_speaker(&mut self, off: bool) {
        self.speaker_off.store(off, Ordering::Relaxed);
    }

    /// Sets speaker playout volume, clamped to 0..=100.
    pub fn set_speaker_volume(&mut self, percent: u8) {
        self.speaker_volume_pct
            .store(percent.min(100), Ordering::Relaxed);
    }

    /// Current speaker playout volume, 0..=100.
    pub fn speaker_volume_percent(&self) -> u8 {
        self.speaker_volume_pct.load(Ordering::Relaxed)
    }

    /// Latest live microphone level (RFC 6464: 0 = loudest, 127 = silence).
    pub fn live_mic_level(&self) -> u8 {
        self.live_mic_level.load(Ordering::Relaxed)
    }

    /// Plays a short pleasant test chime through the currently selected speaker
    /// sink (non-blocking, scaled by speaker volume).
    pub fn play_test_tone(&self) {
        if cfg!(test) || std::env::var_os("ZCALL_NO_PULSE").is_some() {
            return;
        }
        let sink = self.effective_speaker_sink();
        let vol_pct = Arc::clone(&self.speaker_volume_pct);
        let _ = std::thread::Builder::new()
            .name("zcall-spk-test".into())
            .spawn(move || {
                let Ok(sp) = CardSpeaker::open(sink.as_deref())
                    .or_else(|_| CardSpeaker::open(None))
                else {
                    return;
                };
                // 3-note gentle chord chime (C5, E5, G5) over ~1.5 s (75 × 20 ms).
                let notes: &[(f64, usize, usize)] = &[
                    (523.25, 0, 22),
                    (659.25, 18, 46),
                    (783.99, 36, 75),
                ];
                let mut frame = vec![0_i16; FRAME_SAMPLES];
                for f in 0..75_usize {
                    let gain = f64::from(vol_pct.load(Ordering::Relaxed).min(100)) / 100.0;
                    for (i, slot) in frame.iter_mut().enumerate() {
                        let sample_idx = f * FRAME_SAMPLES + i;
                        let t = sample_idx as f64 / 16_000.0;
                        let mut acc = 0.0_f64;
                        for &(freq, start_f, end_f) in notes {
                            if f >= start_f && f < end_f {
                                let local_t = (sample_idx - start_f * FRAME_SAMPLES) as f64 / 16_000.0;
                                let dur = ((end_f - start_f) * FRAME_SAMPLES) as f64 / 16_000.0;
                                let attack = (local_t / 0.025).min(1.0);
                                let decay = (-3.2 * local_t / dur.max(0.1)).exp();
                                let tail = ((dur - local_t) / 0.04).clamp(0.0, 1.0);
                                acc += (2.0 * std::f64::consts::PI * freq * t).sin()
                                    * attack
                                    * decay
                                    * tail;
                            }
                        }
                        *slot = (acc * 7_500.0 * gain).clamp(-32_000.0, 32_000.0) as i16;
                    }
                    if sp.write(&frame).is_err() {
                        break;
                    }
                }
            });
    }

    /// Mutes or unmutes the microphone. Capture keeps running and sends silence.
    pub fn set_mute(&mut self, on: bool) {
        self.muted.store(on, Ordering::Relaxed);
    }

    /// Sets the microphone gain, clamped to 0..=100.
    pub fn set_mic_volume(&mut self, percent: u8) {
        self.volume_pct.store(percent.min(100), Ordering::Relaxed);
    }

    /// Remembers the Pulse level to apply when capture starts.
    ///
    /// Does not mark the slider as manual, so ClipGuard can still step.
    pub fn prepare_os_volume(&mut self, percent: u8) {
        self.os_volume_pct = percent.min(100);
    }

    /// The Pulse level last requested, 0..=100.
    pub fn os_volume_percent(&self) -> u8 {
        self.os_volume_pct
    }

    /// The call slider. Writes Pulse and stops ClipGuard for this call.
    ///
    /// Digital `volume_pct` stays at 100. A second scale would square the cut.
    pub fn set_os_mic_volume(&mut self, percent: u8) {
        self.os_volume_pct = percent.min(100);
        self.clip_manual.store(true, Ordering::Relaxed);
        self.write_os_volume(false);
    }

    /// `wait` blocks until `pactl` returns. Capture start needs that so the
    /// first frames are not still at the old floor.
    fn write_os_volume(&self, wait: bool) {
        if cfg!(test) || std::env::var_os("ZCALL_NO_PULSE").is_some() {
            return;
        }
        let gen = self
            .vol_gen
            .fetch_add(1, Ordering::AcqRel)
            .saturating_add(1);
        let vol_gen = Arc::clone(&self.vol_gen);
        let source = volume_source(self.mic_source.as_deref());
        let percent = self.os_volume_pct;
        let run = move || {
            let _hold = volume_lock().lock().unwrap_or_else(|err| err.into_inner());
            if vol_gen.load(Ordering::Acquire) != gen {
                return;
            }
            pactl_set_source_volume(&source, percent);
        };
        if wait {
            run();
        } else {
            let _ = std::thread::Builder::new()
                .name("zcall-vol".into())
                .spawn(run);
        }
    }

    /// Whether the microphone is muted.
    pub fn is_muted(&self) -> bool {
        self.muted.load(Ordering::Relaxed)
    }

    /// Stops capture and playout.
    pub fn stop(&mut self) {
        self.live_mic_level.store(127, Ordering::Relaxed);
        self.stop_flag.store(true, Ordering::SeqCst);
        if let Some(mut mic) = self.mic.take() {
            mic.close();
        }
        if let Some(handle) = self.cap_handle.take() {
            let _ = handle.join();
        }
        self.stop_playout_thread();
        self.rx_out = None;
        self.far_heard.store(false, Ordering::Relaxed);
        if let Some(mut speaker) = self.speaker.take() {
            speaker.close();
        }
        self.processing = None;
        self.restore_bt_profile();
    }

    /// Switches a PipeWire Bluetooth card from A2DP to HFP/mSBC when capture
    /// targets a `bluez_input.*` / `bluez_source.*` node. Restores the prior
    /// profile when switching to a non-Bluetooth mic. Returns `true` if a card
    /// profile changed (caller should reopen the speaker stream).
    fn ensure_bt_hfp_for_source(&mut self, cap_source: Option<&str>) -> bool {
        if cfg!(test) || std::env::var_os("ZCALL_NO_PULSE").is_some() {
            return false;
        }
        let Some((node_name, device_id)) = resolve_capture_node(cap_source) else {
            self.bt_capture_active.store(false, Ordering::Release);
            return false;
        };
        let is_bt = node_name.starts_with("bluez_input.") || node_name.starts_with("bluez_source.");
        self.bt_capture_active.store(is_bt, Ordering::Release);
        if !is_bt {
            return self.restore_bt_profile();
        }
        let Some(dev_id) = device_id else {
            return false;
        };
        if let Some((prev_dev, _)) = self.bt_restore {
            if prev_dev != dev_id {
                self.restore_bt_profile();
            }
        }
        let Some((active_idx, active_name, hfp_idx, hfp_name)) = find_bt_hfp_switch(dev_id) else {
            return false;
        };
        if active_name.starts_with("headset-head-unit") {
            return false;
        }
        let ok = Command::new("wpctl")
            .args(["set-profile", &dev_id.to_string(), &hfp_idx.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if ok {
            eprintln!(
                "zcall-bt: switched device {dev_id} profile {active_name} ({active_idx}) -> {hfp_name} ({hfp_idx})"
            );
            if self.bt_restore.is_none() {
                self.bt_restore = Some((dev_id, active_idx));
            }
            // Give PipeWire/BlueZ a short moment to bring up the mSBC/CVSD SCO link.
            std::thread::sleep(Duration::from_millis(450));
            true
        } else {
            false
        }
    }

    fn restore_bt_profile(&mut self) -> bool {
        let Some((dev_id, prev_idx)) = self.bt_restore.take() else {
            return false;
        };
        if cfg!(test) || std::env::var_os("ZCALL_NO_PULSE").is_some() {
            return false;
        }
        let ok = Command::new("wpctl")
            .args(["set-profile", &dev_id.to_string(), &prev_idx.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if ok {
            eprintln!("zcall-bt: restored device {dev_id} profile {prev_idx}");
            if self.play_handle.is_some() {
                std::thread::sleep(Duration::from_millis(300));
            }
        }
        ok
    }
}

/// Sound-card playback, or the `pacat` pipe when the library cannot open.
enum PlayDevice {
    /// `write` blocks on the 60 ms stream. The loop must not also sleep 20 ms.
    Card(CardSpeaker),
    /// Software clock. The pipe in front of `pacat` is not the card clock.
    Pipe(Speaker),
}

impl PlayDevice {
    fn paced_by_card(&self) -> bool {
        matches!(self, Self::Card(_))
    }

    fn latency_ms(&self) -> u32 {
        match self {
            Self::Card(card) => card.latency_ms(),
            Self::Pipe(_) => CARD_LATENCY_MS,
        }
    }

    fn write(&mut self, pcm: &[i16]) -> io::Result<()> {
        match self {
            Self::Card(card) => card.write(pcm),
            Self::Pipe(pipe) => pipe.write(pcm),
        }
    }

    fn close(&mut self) {
        if let Self::Pipe(pipe) = self {
            pipe.close();
        }
    }
}

fn open_and_prime_speaker(sink: Option<&str>) -> io::Result<PlayDevice> {
    if std::env::var_os("ZCALL_NO_PULSE").is_none() {
        match CardSpeaker::open(sink).or_else(|err| {
            if sink.is_some() {
                eprintln!("zcall-play: explicit sink failed ({err}); retrying default");
                CardSpeaker::open(None)
            } else {
                Err(err)
            }
        }) {
            Ok(card) => {
                card.prime_silence(CARD_PRIME_FRAMES)?;
                return Ok(PlayDevice::Card(card));
            }
            Err(err) => {
                static NOTED: AtomicBool = AtomicBool::new(false);
                if !NOTED.swap(true, Ordering::Relaxed) {
                    eprintln!("zcall-play: libpulse-simple unavailable ({err}); pacat");
                }
            }
        }
    }
    let mut speaker = Speaker::spawn(&live_speaker_command(sink))?;
    speaker.prime_silence(PLAYOUT_PRIME_FRAMES)?;
    Ok(PlayDevice::Pipe(speaker))
}

fn run_playout_loop(shared: PlayoutShared, ready: mpsc::SyncSender<io::Result<()>>) {
    let mut speaker: Option<PlayDevice> = None;
    let mut seen_epoch = shared.sink_epoch.load(Ordering::Acquire);
    if shared.wanted.load(Ordering::Acquire) {
        let sink = shared
            .sink
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        match open_and_prime_speaker(sink.as_deref()) {
            Ok(sp) => {
                speaker = Some(sp);
                let _ = ready.try_send(Ok(()));
            }
            Err(err) => {
                shared.fault.store(true, Ordering::Release);
                let _ = ready.try_send(Err(err));
                return;
            }
        }
    } else {
        let _ = ready.try_send(Ok(()));
    }

    let mut next = Instant::now() + PLAYOUT_FRAME;
    let mut play_lat = CARD_LATENCY_MS;
    let mut lat_tick = 0_u32;
    while !shared.stop.load(Ordering::Acquire) {
        if !shared.wanted.load(Ordering::Acquire) {
            std::thread::park_timeout(Duration::from_millis(50));
            next = Instant::now() + PLAYOUT_FRAME;
            continue;
        }

        let cur_epoch = shared.sink_epoch.load(Ordering::Acquire);
        if speaker.is_none() || cur_epoch != seen_epoch {
            seen_epoch = cur_epoch;
            let sink = shared
                .sink
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            match open_and_prime_speaker(sink.as_deref()) {
                Ok(new_sp) => {
                    if let Some(mut old) = speaker.replace(new_sp) {
                        old.close();
                    }
                    next = Instant::now() + PLAYOUT_FRAME;
                }
                Err(_) => {
                    shared.fault.store(true, Ordering::Release);
                    return;
                }
            }
        }

        // Card writes block until the 60 ms stream has room, which is the
        // playout clock (`PulseOut._clock` when `paced`). Sleeping another
        // 20 ms on top of that plays at half speed.
        let paced = speaker.as_ref().is_some_and(PlayDevice::paced_by_card);
        if !paced {
            loop {
                if shared.stop.load(Ordering::Acquire)
                    || shared.sink_epoch.load(Ordering::Acquire) != seen_epoch
                {
                    break;
                }
                let now = Instant::now();
                if now >= next {
                    break;
                }
                std::thread::park_timeout(next - now);
            }
            if shared.stop.load(Ordering::Acquire) {
                break;
            }
            if shared.sink_epoch.load(Ordering::Acquire) != seen_epoch {
                continue;
            }
            next += PLAYOUT_FRAME;
            let now_inst = Instant::now();
            if now_inst > next + PLAYOUT_RESYNC {
                next = now_inst + PLAYOUT_FRAME;
            }
        } else if shared.stop.load(Ordering::Acquire) {
            break;
        }

        let now_secs = monotonic_secs();
        let speaker_off = shared.speaker_off.load(Ordering::Relaxed);
        let spk_vol = shared.speaker_volume_pct.load(Ordering::Relaxed).min(100);
        let play = {
            let mut mixer = shared.mixer.lock().unwrap_or_else(|e| e.into_inner());
            let mixed = mixer.mix(now_secs);
            let mut play = if speaker_off || spk_vol == 0 {
                vec![0_i16; FRAME_SAMPLES]
            } else {
                mixed
            };
            if !speaker_off && spk_vol > 0 && spk_vol < 100 {
                let scale = i32::from(spk_vol);
                for s in &mut play {
                    *s = ((i32::from(*s) * scale) / 100).clamp(-32_768, 32_767) as i16;
                }
            }
            mixer.note_played(&play, now_secs);
            shared
                .far_heard
                .store(mixer.far_active(now_secs), Ordering::Relaxed);
            play
        };
        if let Some(sp) = speaker.as_mut() {
            if shared.use_in_process_apm.load(Ordering::Acquire) {
                if lat_tick % 25 == 0 {
                    play_lat = sp.latency_ms();
                }
                lat_tick = lat_tick.wrapping_add(1);
                shared.apm.feed_render(&play, play_lat);
            }
            if sp.write(&play).is_err() {
                shared.fault.store(true, Ordering::Release);
                sp.close();
                return;
            }
        }
    }
    if let Some(mut sp) = speaker.take() {
        sp.close();
    }
}

/// Opt-in flag to fall back to the legacy external `pipewire -c` echo canceller.
fn legacy_pipewire_aec_requested() -> bool {
    std::env::var("ZCALL_LEGACY_PW_AEC").ok().as_deref() == Some("1")
}

/// Empty and the localized default settings label both mean the system default.
fn named(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != "Mặc định")
        .map(str::to_owned)
}

/// One 20 ms PCM frame from a `parec` stdout.
fn read_pcm_frame(stdout: &mut impl Read) -> Option<Vec<u8>> {
    let mut frame = vec![0_u8; FRAME_BYTES];
    let mut filled = 0;
    while filled < FRAME_BYTES {
        match stdout.read(&mut frame[filled..]) {
            Ok(0) => return None,
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return None,
        }
    }
    Some(frame)
}

/// RFC 6464 audio level: 0 (full scale) .. 127 (silence).
fn audio_level(pcm: &[i16]) -> u8 {
    if pcm.is_empty() {
        return 127;
    }
    let sum: f64 = pcm.iter().map(|v| f64::from(*v) * f64::from(*v)).sum();
    let rms = (sum / pcm.len() as f64).sqrt();
    if rms < 1.0 {
        return 127;
    }
    let dbov = -20.0 * (rms / 32768.0).log10();
    (dbov.round() as i64).clamp(0, 127) as u8
}

/// Seconds since this process started.
///
/// Shared by the microphone, the jitter arrival times and playout. A fresh
/// `Instant::now().elapsed()` is about zero every call, so a far-end hold of
/// 0.35 s would stay true forever and the clip guard would never step.
pub fn monotonic_secs() -> f64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64()
}

fn volume_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn pactl_set_source_volume(source: &str, percent: u8) {
    let pct = percent.min(100);
    let raw = percent_to_raw(pct).to_string();
    let pactl_ok = Command::new("pactl")
        .args(["set-source-volume", source, &raw])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if !pactl_ok {
        let target = if source == "@DEFAULT_SOURCE@" {
            "@DEFAULT_AUDIO_SOURCE@".to_owned()
        } else {
            pipewire_node_id_by_name(source)
                .map(|id| id.to_string())
                .unwrap_or_else(|| "@DEFAULT_AUDIO_SOURCE@".to_owned())
        };
        let frac = format!("{:.2}", f64::from(pct) / 100.0);
        let _ = Command::new("wpctl")
            .args(["set-volume", &target, &frac])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

fn pipewire_node_id_by_name(node_name: &str) -> Option<u32> {
    let out = Command::new("pw-dump")
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let list = serde_json::from_slice::<Vec<Value>>(&out.stdout).ok()?;
    for item in list {
        if item.get("type").and_then(Value::as_str) != Some("PipeWire:Interface:Node") {
            continue;
        }
        if item.pointer("/info/props/node.name").and_then(Value::as_str) == Some(node_name) {
            return item.get("id").and_then(Value::as_u64).and_then(|v| u32::try_from(v).ok());
        }
    }
    None
}

/// Resolves `(node.name, device.id)` for the active capture source (explicit or
/// PipeWire's `@DEFAULT_AUDIO_SOURCE@`).
fn resolve_capture_node(explicit: Option<&str>) -> Option<(String, Option<u32>)> {
    if let Some(name) = explicit.filter(|s| !s.is_empty()) {
        let out = Command::new("pw-dump")
            .stderr(Stdio::null())
            .output()
            .ok()?;
        if !out.status.success() {
            return Some((name.to_owned(), None));
        }
        let list = serde_json::from_slice::<Vec<Value>>(&out.stdout).ok()?;
        for item in list {
            if item.get("type").and_then(Value::as_str) != Some("PipeWire:Interface:Node") {
                continue;
            }
            if item.pointer("/info/props/node.name").and_then(Value::as_str) == Some(name) {
                let dev_id = item
                    .pointer("/info/props/device.id")
                    .and_then(|v| {
                        v.as_u64()
                            .or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()))
                    })
                    .and_then(|v| u32::try_from(v).ok());
                return Some((name.to_owned(), dev_id));
            }
        }
        return Some((name.to_owned(), None));
    }
    let out = Command::new("wpctl")
        .args(["inspect", "@DEFAULT_AUDIO_SOURCE@"])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_wpctl_inspect_node(&String::from_utf8_lossy(&out.stdout))
}

fn parse_wpctl_inspect_node(text: &str) -> Option<(String, Option<u32>)> {
    let mut node_name = None;
    let mut device_id = None;
    for line in text.lines() {
        let trimmed = line.trim().trim_start_matches('*').trim();
        if let Some(rest) = trimmed.strip_prefix("node.name = ") {
            node_name = Some(rest.trim().trim_matches('"').to_owned());
        } else if let Some(rest) = trimmed.strip_prefix("device.id = ") {
            device_id = rest.trim().trim_matches('"').parse::<u32>().ok();
        }
    }
    node_name.map(|name| (name, device_id))
}

/// Reads active profile and preferred HFP profile `(active_idx, active_name,
/// hfp_idx, hfp_name)` from `pw-dump <device_id>`.
fn find_bt_hfp_switch(device_id: u32) -> Option<(u32, String, u32, String)> {
    let out = Command::new("pw-dump")
        .arg(device_id.to_string())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let list = serde_json::from_slice::<Vec<Value>>(&out.stdout).ok()?;
    parse_bt_hfp_switch(&list)
}

fn parse_bt_hfp_switch(list: &[Value]) -> Option<(u32, String, u32, String)> {
    let dev = list.first()?;
    let params = dev.pointer("/info/params")?;
    let active = params.get("Profile")?.as_array()?.first()?;
    let active_idx = active.get("index")?.as_u64().and_then(|v| u32::try_from(v).ok())?;
    let active_name = active.get("name")?.as_str()?.to_owned();
    let profiles = params.get("EnumProfile")?.as_array()?;
    let mut best: Option<(u8, u32, String)> = None;
    for p in profiles {
        if p.get("available").and_then(Value::as_str) == Some("no") {
            continue;
        }
        let Some(name) = p.get("name").and_then(Value::as_str) else {
            continue;
        };
        let Some(idx) = p.get("index").and_then(Value::as_u64).and_then(|v| u32::try_from(v).ok()) else {
            continue;
        };
        let rank = if name == "headset-head-unit" {
            2
        } else if name.starts_with("headset-head-unit") {
            1
        } else {
            0
        };
        if rank > 0 && best.as_ref().is_none_or(|(r, _, _)| rank > *r) {
            best = Some((rank, idx, name.to_owned()));
        }
    }
    let (_, hfp_idx, hfp_name) = best?;
    Some((active_idx, active_name, hfp_idx, hfp_name))
}

/// Pulse source for `pactl`. An empty selection is the system default.
fn volume_source(name: Option<&str>) -> String {
    name.filter(|s| !s.is_empty())
        .unwrap_or("@DEFAULT_SOURCE@")
        .to_owned()
}

/// The first `… / -6.00 dB` value in `pactl get-source-volume` output.
fn parse_volume_db(text: &str) -> Option<f64> {
    let before = text.split(" dB").next()?;
    let token = before.rsplit('/').next()?.trim();
    token.parse().ok()
}

fn read_source_db(source: &str) -> Option<f64> {
    let out = Command::new("pactl")
        .args(["get-source-volume", source])
        .env("LC_ALL", "C")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_volume_db(&String::from_utf8_lossy(&out.stdout))
}

/// Applies one ClipGuard step with `pactl`, off the microphone thread.
///
/// At most one step runs at a time. A busy helper drops this step; `feed`
/// already recorded it, and the next window is a second later.
fn request_volume_step(
    guard: &Arc<Mutex<ClipGuard>>,
    busy: &Arc<AtomicBool>,
    source: &str,
    kind: StepKind,
) {
    if busy
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
        .is_err()
    {
        return;
    }
    let guard = Arc::clone(guard);
    let busy_thread = Arc::clone(busy);
    let source = source.to_owned();
    let spawned = std::thread::Builder::new()
        .name("zcall-clip".into())
        .spawn(move || {
            let cur = read_source_db(&source);
            let target = guard
                .lock()
                .unwrap_or_else(|err| err.into_inner())
                .step(kind, cur);
            if let Some(db) = target {
                let raw = db_to_raw(db).to_string();
                let _ = Command::new("pactl")
                    .args(["set-source-volume", &source, &raw])
                    .status();
            }
            busy_thread.store(false, Ordering::Release);
        });
    if spawned.is_err() {
        busy.store(false, Ordering::Release);
    }
}

/// Silence when muted, otherwise scale by `percent` (0..=100).
fn apply_mic(samples: Vec<i16>, muted: bool, percent: u8) -> Vec<i16> {
    if muted || percent == 0 {
        return vec![0; samples.len()];
    }
    if percent >= 100 {
        return samples;
    }
    let gain = f32::from(percent) / 100.0;
    samples
        .into_iter()
        .map(|sample| (f32::from(sample) * gain) as i16)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mute_and_volume_change_the_frame() {
        let frame = vec![1000, -1000, 500];
        assert_eq!(apply_mic(frame.clone(), true, 100), vec![0, 0, 0]);
        assert_eq!(apply_mic(frame.clone(), false, 0), vec![0, 0, 0]);
        assert_eq!(apply_mic(frame.clone(), false, 100), frame);
        assert_eq!(apply_mic(frame, false, 50), vec![500, -500, 250]);

        let bridge = AudioBridge::new();
        assert!(!bridge.is_muted());
        let mut bridge = bridge;
        bridge.set_mute(true);
        bridge.set_mic_volume(140);
        bridge.set_speaker(true);
        assert!(bridge.is_muted());
        assert!(bridge.speaker_off.load(Ordering::Relaxed));
        assert_eq!(bridge.volume_pct.load(Ordering::Relaxed), 100);
    }

    #[test]
    fn os_volume_does_not_scale_the_pcm() {
        let mut bridge = AudioBridge::new();
        bridge.prepare_os_volume(80);
        assert_eq!(bridge.os_volume_percent(), 80);
        assert!(!bridge.clip_manual.load(Ordering::Relaxed));
        assert_eq!(bridge.volume_pct.load(Ordering::Relaxed), 100);
        bridge.set_os_mic_volume(140);
        assert_eq!(bridge.os_volume_percent(), 100);
        assert!(bridge.clip_manual.load(Ordering::Relaxed));
        assert_eq!(bridge.volume_pct.load(Ordering::Relaxed), 100);
        assert_eq!(percent_to_raw(40), 26214);
    }

    #[test]
    fn device_names_are_remembered_before_capture_starts() {
        let mut bridge = AudioBridge::new();
        bridge.set_source(Some("alsa_input.usb")).unwrap();
        bridge.set_sink(Some("Mặc định")).unwrap();
        assert_eq!(bridge.mic_source.as_deref(), Some("alsa_input.usb"));
        assert!(bridge.speaker_sink.is_none());
        bridge.set_source(Some("")).unwrap();
        assert!(bridge.mic_source.is_none());
    }

    #[test]
    fn pactl_volume_line_is_decibels() {
        let line =
            "Volume: front-left: 65536 / 100% / 0.00 dB,   front-right: 46396 /  71% / -9.00 dB";
        assert_eq!(parse_volume_db(line), Some(0.0));
        assert_eq!(
            parse_volume_db("Volume: mono: 32872 / 50% / -18.06 dB"),
            Some(-18.06)
        );
        assert_eq!(parse_volume_db("no volume"), None);
        assert_eq!(volume_source(None), "@DEFAULT_SOURCE@");
        assert_eq!(volume_source(Some("alsa_input.usb")), "alsa_input.usb");
    }

    #[test]
    fn jitter_window_follows_max_audio_delay() {
        let mut bridge = AudioBridge::new();
        bridge.set_jitter_ms(100, 2000);
        assert_eq!(bridge.jitter_min, 5);
        assert_eq!(bridge.jitter_max, 100);
        assert!(bridge.play_handle.is_none());
    }

    #[test]
    fn apply_config_updates_group_encoder_and_jitter() {
        let mut bridge = AudioBridge::new();
        let cfg = serde_json::json!({
            "minAudioDelayMs": 120,
            "maxAudioDelayMs": 1000,
            "audioBitrate": 20,
            "opusComplex": 6,
            "audioFecInband": false,
            "audioEchoCancellation": true,
            "audioNoiseSuppression": false,
            "audioHighPassFilter": false,
            "audioVoiceDetection": true,
        });
        bridge.apply_config(true, &cfg);
        assert_eq!(bridge.jitter_min, 6);
        assert_eq!(bridge.jitter_max, 50);
        assert_eq!(bridge.encoder_cfg.bitrate_bps, 20_000);
        assert_eq!(bridge.encoder_cfg.complexity, 6);
        assert!(!bridge.encoder_cfg.inband_fec);
        assert!(!bridge.high_pass);
        assert!(bridge.proc_opts.aec);
        assert!(!bridge.proc_opts.noise_suppression);
        assert!(!bridge.proc_opts.high_pass);
        assert!(bridge.proc_opts.voice_detection);

        // 1-1 keeps default 32 kbps / FEC on even if group keys are present.
        let mut one_to_one = AudioBridge::new();
        one_to_one.apply_config(false, &cfg);
        assert_eq!(one_to_one.encoder_cfg, OpusEncoderConfig::default());
        assert_eq!(one_to_one.jitter_max, 50);
    }

    #[test]
    fn parses_wpctl_default_source_and_bt_hfp_profile() {
        let inspect = "\
id 117, type PipeWire:Interface:Node
  * device.id = \"113\"
  * node.name = \"bluez_input.24:64:9F:3B:BB:12\"
";
        assert_eq!(
            parse_wpctl_inspect_node(inspect),
            Some(("bluez_input.24:64:9F:3B:BB:12".to_owned(), Some(113)))
        );

        let dump = vec![serde_json::json!({
            "id": 113,
            "info": {
                "params": {
                    "Profile": [{ "index": 131073, "name": "a2dp-sink" }],
                    "EnumProfile": [
                        { "index": 131073, "name": "a2dp-sink", "available": "yes" },
                        { "index": 196864, "name": "headset-head-unit-cvsd", "available": "yes" },
                        { "index": 196865, "name": "headset-head-unit", "available": "yes" }
                    ]
                }
            }
        })];
        assert_eq!(
            parse_bt_hfp_switch(&dump),
            Some((
                131073,
                "a2dp-sink".to_owned(),
                196865,
                "headset-head-unit".to_owned()
            ))
        );
    }
}
