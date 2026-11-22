//! Stand-in used when this crate is type-checked off Windows. Devices are not opened.

use std::io;

/// Bytes of one 20 ms, 16 kHz mono `i16` frame.
pub const FRAME_BYTES: usize = 640;
/// Samples in [`FRAME_BYTES`].
pub const FRAME_SAMPLES: usize = 320;
/// Target output buffer, in milliseconds.
pub const CARD_LATENCY_MS: u32 = 60;
/// Target capture fragment, in milliseconds.
pub const CARD_MIC_LATENCY_MS: u32 = 20;

/// No devices are enumerated off Windows.
#[must_use]
pub fn list_endpoints(_kind: &str) -> Vec<(String, String)> {
    Vec::new()
}

/// Playback handle. [`Self::open`] fails off Windows.
pub struct CardSpeaker;

impl CardSpeaker {
    /// # Errors
    ///
    /// Always fails off Windows.
    pub fn open(device: Option<&str>) -> Result<Self, io::Error> {
        Self::open_spec(device, 16_000, 1, "Call")
    }

    /// # Errors
    ///
    /// Always fails off Windows.
    pub fn open_spec(
        _device: Option<&str>,
        _rate: u32,
        _channels: u8,
        _stream_name: &str,
    ) -> Result<Self, io::Error> {
        Err(io::Error::other("WASAPI playback is only built on Windows"))
    }

    /// # Errors
    ///
    /// Always fails off Windows.
    pub fn write(&self, _pcm: &[i16]) -> Result<(), io::Error> {
        Err(io::Error::other("WASAPI playback is only built on Windows"))
    }

    /// # Errors
    ///
    /// Always fails off Windows.
    pub fn prime_silence(&self, _frames: usize) -> Result<(), io::Error> {
        Err(io::Error::other("WASAPI playback is only built on Windows"))
    }

    /// Nominal latency.
    #[must_use]
    pub fn latency_ms(&self) -> u32 {
        CARD_LATENCY_MS
    }
}

/// Microphone handle. [`Self::open`] fails off Windows.
pub struct CardMic;

impl CardMic {
    /// # Errors
    ///
    /// Always fails off Windows.
    pub fn open(_device: Option<&str>) -> Result<Self, io::Error> {
        Err(io::Error::other("WASAPI capture is only built on Windows"))
    }

    /// # Errors
    ///
    /// Always fails off Windows.
    pub fn read_frame(&self) -> Result<Vec<i16>, io::Error> {
        Err(io::Error::other("WASAPI capture is only built on Windows"))
    }

    /// Nominal latency.
    #[must_use]
    pub fn latency_ms(&self) -> u32 {
        CARD_MIC_LATENCY_MS
    }
}

/// Fallback capture. Unused on Windows and off Windows.
pub struct MicCapture;

impl MicCapture {
    /// # Errors
    ///
    /// Always fails.
    pub fn spawn(_command: &[String]) -> Result<Self, io::Error> {
        Err(io::Error::other("WASAPI capture is only built on Windows"))
    }

    /// # Errors
    ///
    /// Always fails.
    pub fn from_mic(
        _device: Option<&str>,
        _source: Option<&str>,
        _pulse_running: bool,
    ) -> Result<Self, io::Error> {
        Err(io::Error::other("WASAPI capture is only built on Windows"))
    }

    /// No frames.
    #[must_use]
    pub fn next_frame(&mut self) -> Option<Vec<u8>> {
        None
    }

    /// No process.
    pub fn close(&mut self) {}
}

/// Fallback speaker.
pub struct Speaker;

impl Speaker {
    /// # Errors
    ///
    /// Always fails.
    pub fn from_sink(_sink: Option<&str>) -> Result<Self, io::Error> {
        Err(io::Error::other("WASAPI playback is only built on Windows"))
    }

    /// # Errors
    ///
    /// Always fails.
    pub fn write(&mut self, _pcm: &[i16]) -> Result<(), io::Error> {
        Err(io::Error::other("WASAPI playback is only built on Windows"))
    }

    /// No process.
    pub fn close(&mut self) {}
}
