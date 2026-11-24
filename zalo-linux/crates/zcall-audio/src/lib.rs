//! Audio capture and playout for the Linux Rust Zalo call engine.
//!
//! Hardware I/O runs natively through `libpulse-simple` at 48 kHz (matching
//! PipeWire's clock with zero daemon resampling) and falls back cleanly to
//! 16 kHz `libpulse-simple` or `parec`/`pacat`/`pw-record`/`arecord`.
//! Shared DSP (`apm`, `jitter`, `mixer`, `resampler`, `wsola`) is re-exported
//! from `zcall-dsp`.

#![forbid(unsafe_code)]

pub mod buffer;
pub mod capture;
pub mod pipeline;
pub mod processing;

pub use zcall_dsp::{apm, jitter, mixer, resampler, wsola};

pub use apm::InProcessApm;
pub use buffer::FrameBuffer;
pub use capture::{
    mic_command, speaker_command, CardMic, CardSpeaker, MicCapture, Speaker, CARD_LATENCY_MS,
    CARD_MIC_LATENCY_MS, FRAME_BYTES, NATIVE_FRAME_BYTES, NATIVE_FRAME_SAMPLES, NATIVE_RATE,
};
pub use mixer::{playout_step, run_playout, Mixer, PcmSink};
pub use pipeline::{bytes_to_i16, AudioPipeline};
pub use processing::AudioProcessing;
pub use resampler::PolyphaseResampler;
