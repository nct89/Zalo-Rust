//! CoreAudio (via `cpal`) capture and playout.
//!
//! Call audio is 16 kHz mono `i16`. Device streams stay at the hardware rate
//! and are resampled in this crate. Media playback uses [`CardSpeaker::open_spec`]
//! with the file's own rate and channel count.

use std::collections::VecDeque;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, Stream, StreamConfig};

use crate::resample_to_device;

/// Bytes of one 20 ms, 16 kHz mono `i16` frame.
pub const FRAME_BYTES: usize = 640;

/// Samples in [`FRAME_BYTES`].
pub const FRAME_SAMPLES: usize = 320;

/// Target output buffer, in milliseconds.
pub const CARD_LATENCY_MS: u32 = 60;

/// Target capture fragment, in milliseconds.
pub const CARD_MIC_LATENCY_MS: u32 = 20;

const CALL_RATE: u32 = 16_000;

/// `(id, label)` for Settings. `kind` is `"sinks"` or `"sources"`.
#[must_use]
pub fn list_endpoints(kind: &str) -> Vec<(String, String)> {
    if cfg!(test) {
        return Vec::new();
    }
    let host = cpal::default_host();
    let devices = if kind == "sinks" {
        host.output_devices()
    } else {
        host.input_devices()
    };
    let Ok(devices) = devices else {
        return Vec::new();
    };
    devices
        .filter_map(|device| device.name().ok())
        .filter(|name| !name.is_empty())
        .map(|name| (name.clone(), name))
        .collect()
}

/// Playback. `write` blocks while the device buffer is full.
pub struct CardSpeaker {
    stream: Option<Stream>,
    queue: Arc<Mutex<VecDeque<f32>>>,
    space: Arc<Condvar>,
    closed: Arc<AtomicBool>,
    out_rate: u32,
    out_channels: u16,
    in_rate: u32,
    in_channels: u8,
}

impl CardSpeaker {
    /// Opens the default output, or `device` when its name matches, at 16 kHz mono.
    ///
    /// # Errors
    ///
    /// Returns an error when CoreAudio has no output device.
    pub fn open(device: Option<&str>) -> Result<Self, io::Error> {
        Self::open_spec(device, CALL_RATE, 1, "Call")
    }

    /// Opens output for interleaved `i16` PCM at `rate` / `channels`.
    ///
    /// # Errors
    ///
    /// Returns an error when the device cannot start a stream.
    pub fn open_spec(
        device: Option<&str>,
        rate: u32,
        channels: u8,
        _stream_name: &str,
    ) -> Result<Self, io::Error> {
        if rate == 0 || channels == 0 {
            return Err(io::Error::other("invalid playback spec"));
        }
        let host = cpal::default_host();
        let output = pick_device(host.output_devices().ok(), device)?
            .or_else(|| host.default_output_device())
            .ok_or_else(|| io::Error::other("no output device"))?;
        let supported = output
            .default_output_config()
            .map_err(|err| io::Error::other(err.to_string()))?;
        let out_rate = supported.sample_rate().0;
        let out_channels = supported.channels();
        let format = supported.sample_format();
        let config: StreamConfig = supported.into();
        let queue = Arc::new(Mutex::new(VecDeque::new()));
        let space = Arc::new(Condvar::new());
        let closed = Arc::new(AtomicBool::new(false));
        let stream = build_output(
            &output,
            &config,
            format,
            Arc::clone(&queue),
            Arc::clone(&space),
            Arc::clone(&closed),
        )?;
        stream
            .play()
            .map_err(|err| io::Error::other(err.to_string()))?;
        Ok(Self {
            stream: Some(stream),
            queue,
            space,
            closed,
            out_rate,
            out_channels,
            in_rate: rate,
            in_channels: channels,
        })
    }

    /// Writes PCM. Blocks while more than [`CARD_LATENCY_MS`] is queued.
    ///
    /// # Errors
    ///
    /// Returns an error when the stream has been closed.
    pub fn write(&self, pcm: &[i16]) -> Result<(), io::Error> {
        if pcm.is_empty() {
            return Ok(());
        }
        let samples = resample_to_device(
            pcm,
            self.in_rate,
            self.in_channels,
            self.out_rate,
            self.out_channels,
        );
        let cap = queued_cap(self.out_rate, self.out_channels, CARD_LATENCY_MS);
        let mut guard = self.queue.lock().unwrap_or_else(|err| err.into_inner());
        while guard.len() + samples.len() > cap && !self.closed.load(Ordering::Acquire) {
            let (next, timeout) = self
                .space
                .wait_timeout(guard, Duration::from_millis(80))
                .unwrap_or_else(|err| err.into_inner());
            guard = next;
            if timeout.timed_out() && guard.len() + samples.len() > cap {
                return Err(io::Error::other("speaker write timed out"));
            }
        }
        if self.closed.load(Ordering::Acquire) {
            return Err(io::Error::other("speaker closed"));
        }
        guard.extend(samples);
        Ok(())
    }

    /// Queues `frames` of 20 ms silence.
    ///
    /// # Errors
    ///
    /// Returns the error from [`Self::write`].
    pub fn prime_silence(&self, frames: usize) -> Result<(), io::Error> {
        let zeros = vec![0_i16; FRAME_SAMPLES];
        for _ in 0..frames {
            self.write(&zeros)?;
        }
        Ok(())
    }

    /// Nominal output latency.
    #[must_use]
    pub fn latency_ms(&self) -> u32 {
        CARD_LATENCY_MS
    }
}

impl Drop for CardSpeaker {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::Release);
        self.space.notify_all();
        self.stream.take();
    }
}

/// Microphone at 16 kHz mono.
pub struct CardMic {
    stream: Option<Stream>,
    queue: Arc<Mutex<VecDeque<i16>>>,
    ready: Arc<Condvar>,
    closed: Arc<AtomicBool>,
}

impl CardMic {
    /// Opens the default input, or `device` when its name matches.
    ///
    /// # Errors
    ///
    /// Returns an error when CoreAudio has no input device.
    pub fn open(device: Option<&str>) -> Result<Self, io::Error> {
        let host = cpal::default_host();
        let input = pick_device(host.input_devices().ok(), device)?
            .or_else(|| host.default_input_device())
            .ok_or_else(|| io::Error::other("no input device"))?;
        let supported = input
            .default_input_config()
            .map_err(|err| io::Error::other(err.to_string()))?;
        let in_rate = supported.sample_rate().0;
        let in_channels = supported.channels();
        let format = supported.sample_format();
        let config: StreamConfig = supported.into();
        let queue = Arc::new(Mutex::new(VecDeque::new()));
        let ready = Arc::new(Condvar::new());
        let closed = Arc::new(AtomicBool::new(false));
        let stream = build_input(
            &input,
            &config,
            format,
            in_rate,
            in_channels,
            Arc::clone(&queue),
            Arc::clone(&ready),
            Arc::clone(&closed),
        )?;
        stream
            .play()
            .map_err(|err| io::Error::other(err.to_string()))?;
        Ok(Self {
            stream: Some(stream),
            queue,
            ready,
            closed,
        })
    }

    /// Blocks until one 20 ms frame is available.
    ///
    /// # Errors
    ///
    /// Returns an error when the stream closes or the wait times out.
    pub fn read_frame(&self) -> Result<Vec<i16>, io::Error> {
        let mut guard = self.queue.lock().unwrap_or_else(|err| err.into_inner());
        let start = std::time::Instant::now();
        while guard.len() < FRAME_SAMPLES {
            if self.closed.load(Ordering::Acquire) {
                return Err(io::Error::other("microphone closed"));
            }
            if start.elapsed() > Duration::from_secs(2) {
                return Err(io::Error::other("microphone read timed out"));
            }
            let (next, _) = self
                .ready
                .wait_timeout(guard, Duration::from_millis(50))
                .unwrap_or_else(|err| err.into_inner());
            guard = next;
        }
        Ok(guard.drain(..FRAME_SAMPLES).collect())
    }

    /// Nominal capture latency.
    #[must_use]
    pub fn latency_ms(&self) -> u32 {
        CARD_MIC_LATENCY_MS
    }
}

impl Drop for CardMic {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::Release);
        self.ready.notify_all();
        self.stream.take();
    }
}

/// Fallback capture. CoreAudio uses [`CardMic`]; this stays for the voice-message fallback path.
pub struct MicCapture;

impl MicCapture {
    /// Not used on macOS.
    ///
    /// # Errors
    ///
    /// Always returns an error. Call [`CardMic::open`] instead.
    pub fn spawn(_command: &[String]) -> Result<Self, io::Error> {
        Err(io::Error::other("macOS capture uses CoreAudio"))
    }

    /// Not used on macOS.
    ///
    /// # Errors
    ///
    /// Always returns an error. Call [`CardMic::open`] instead.
    pub fn from_mic(
        _device: Option<&str>,
        _source: Option<&str>,
        _pulse_running: bool,
    ) -> Result<Self, io::Error> {
        Err(io::Error::other("macOS capture uses CoreAudio"))
    }

    /// No subprocess frames.
    #[must_use]
    pub fn next_frame(&mut self) -> Option<Vec<u8>> {
        None
    }

    /// No subprocess to close.
    pub fn close(&mut self) {}
}

/// Speaker fallback type kept so call sites that name it still compile.
pub struct Speaker;

impl Speaker {
    /// Not used on macOS.
    ///
    /// # Errors
    ///
    /// Always returns an error. Call [`CardSpeaker::open`] instead.
    pub fn from_sink(_sink: Option<&str>) -> Result<Self, io::Error> {
        Err(io::Error::other("macOS playout uses CoreAudio"))
    }

    /// Not used on macOS.
    ///
    /// # Errors
    ///
    /// Always returns an error.
    pub fn write(&mut self, _pcm: &[i16]) -> Result<(), io::Error> {
        Err(io::Error::other("macOS playout uses CoreAudio"))
    }

    /// Closes nothing.
    pub fn close(&mut self) {}
}

fn pick_device<I>(
    devices: Option<I>,
    name: Option<&str>,
) -> Result<Option<cpal::Device>, io::Error>
where
    I: IntoIterator<Item = cpal::Device>,
{
    let Some(name) = name.map(str::trim).filter(|name| !name.is_empty() && *name != "Mặc định")
    else {
        return Ok(None);
    };
    let Some(devices) = devices else {
        return Ok(None);
    };
    for device in devices {
        if device.name().ok().as_deref() == Some(name) {
            return Ok(Some(device));
        }
    }
    Err(io::Error::other(format!("audio device not found: {name}")))
}

fn queued_cap(rate: u32, channels: u16, latency_ms: u32) -> usize {
    let frames = (u64::from(rate) * u64::from(latency_ms) / 1000).max(1);
    (frames as usize) * usize::from(channels.max(1))
}

struct InputState {
    phase: f64,
    pending: Vec<i16>,
}

fn consume_device_input(
    state: &mut InputState,
    device_rate: u32,
    channels: u16,
    samples: &[f32],
    queue: &Mutex<VecDeque<i16>>,
    ready: &Condvar,
) {
    if device_rate == 0 || channels == 0 {
        return;
    }
    let step = f64::from(CALL_RATE) / f64::from(device_rate);
    for frame in samples.chunks(usize::from(channels)) {
        let sum: f32 = frame.iter().copied().sum();
        let mono = sum / f32::from(channels);
        state.phase += step;
        if state.phase >= 1.0 {
            state.phase -= 1.0;
            let sample = (mono.clamp(-1.0, 1.0) * 32767.0) as i16;
            state.pending.push(sample);
        }
    }
    if state.pending.len() >= FRAME_SAMPLES {
        push_i16(queue, ready, &state.pending);
        state.pending.clear();
    }
}

fn push_i16(queue: &Mutex<VecDeque<i16>>, ready: &Condvar, samples: &[i16]) {
    if let Ok(mut guard) = queue.lock() {
        guard.extend(samples.iter().copied());
        let cap = FRAME_SAMPLES * 8;
        if guard.len() > cap {
            let drop_n = guard.len() - cap;
            guard.drain(..drop_n);
        }
        ready.notify_one();
    }
}

fn build_output(
    device: &cpal::Device,
    config: &StreamConfig,
    format: SampleFormat,
    queue: Arc<Mutex<VecDeque<f32>>>,
    space: Arc<Condvar>,
    closed: Arc<AtomicBool>,
) -> Result<Stream, io::Error> {
    let err_fn = |err| eprintln!("zalo-gpui speaker: {err}");
    let stream = match format {
        SampleFormat::F32 => device.build_output_stream(
            config,
            move |data: &mut [f32], _| fill_f32(data, &queue, &space, &closed),
            err_fn,
            None,
        ),
        SampleFormat::I16 => device.build_output_stream(
            config,
            move |data: &mut [i16], _| fill_i16(data, &queue, &space, &closed),
            err_fn,
            None,
        ),
        SampleFormat::U16 => device.build_output_stream(
            config,
            move |data: &mut [u16], _| fill_u16(data, &queue, &space, &closed),
            err_fn,
            None,
        ),
        other => {
            return Err(io::Error::other(format!("unsupported output format {other:?}")));
        }
    };
    stream.map_err(|err| io::Error::other(err.to_string()))
}

fn fill_f32(data: &mut [f32], queue: &Mutex<VecDeque<f32>>, space: &Condvar, closed: &AtomicBool) {
    if closed.load(Ordering::Acquire) {
        data.fill(0.0);
        space.notify_all();
        return;
    }
    if let Ok(mut guard) = queue.lock() {
        for sample in data.iter_mut() {
            *sample = guard.pop_front().unwrap_or(0.0);
        }
        space.notify_one();
    }
}

fn fill_i16(data: &mut [i16], queue: &Mutex<VecDeque<f32>>, space: &Condvar, closed: &AtomicBool) {
    if let Ok(mut guard) = queue.lock() {
        for sample in data.iter_mut() {
            if closed.load(Ordering::Acquire) {
                *sample = 0;
                continue;
            }
            let value = guard.pop_front().unwrap_or(0.0);
            *sample = (value.clamp(-1.0, 1.0) * 32767.0) as i16;
        }
        space.notify_one();
    }
}

fn fill_u16(data: &mut [u16], queue: &Mutex<VecDeque<f32>>, space: &Condvar, closed: &AtomicBool) {
    if let Ok(mut guard) = queue.lock() {
        for sample in data.iter_mut() {
            if closed.load(Ordering::Acquire) {
                *sample = 32768;
                continue;
            }
            let value = guard.pop_front().unwrap_or(0.0);
            let shifted = (value.clamp(-1.0, 1.0) * 32767.0) as i32 + 32768;
            *sample = shifted.clamp(0, u16::MAX as i32) as u16;
        }
        space.notify_one();
    }
}

fn build_input(
    device: &cpal::Device,
    config: &StreamConfig,
    format: SampleFormat,
    device_rate: u32,
    channels: u16,
    queue: Arc<Mutex<VecDeque<i16>>>,
    ready: Arc<Condvar>,
    closed: Arc<AtomicBool>,
) -> Result<Stream, io::Error> {
    let err_fn = |err| eprintln!("zalo-gpui mic: {err}");
    let stream = match format {
        SampleFormat::F32 => {
            let state = Arc::new(Mutex::new(InputState {
                phase: 0.0,
                pending: Vec::new(),
            }));
            device.build_input_stream(
                config,
                move |data: &[f32], _| {
                    if closed.load(Ordering::Acquire) {
                        return;
                    }
                    if let Ok(mut guard) = state.lock() {
                        consume_device_input(&mut guard, device_rate, channels, data, &queue, &ready);
                    }
                },
                err_fn,
                None,
            )
        }
        SampleFormat::I16 => {
            let state = Arc::new(Mutex::new(InputState {
                phase: 0.0,
                pending: Vec::new(),
            }));
            device.build_input_stream(
                config,
                move |data: &[i16], _| {
                    if closed.load(Ordering::Acquire) {
                        return;
                    }
                    let floats: Vec<f32> = data
                        .iter()
                        .map(|sample| f32::from(*sample) / 32768.0)
                        .collect();
                    if let Ok(mut guard) = state.lock() {
                        consume_device_input(
                            &mut guard,
                            device_rate,
                            channels,
                            &floats,
                            &queue,
                            &ready,
                        );
                    }
                },
                err_fn,
                None,
            )
        }
        other => {
            return Err(io::Error::other(format!("unsupported input format {other:?}")));
        }
    };
    stream.map_err(|err| io::Error::other(err.to_string()))
}
