//! Audio capture and playout for the Rust Zalo call engine.
//!
//! Ported from the reference call implementation (`audio-io.py`): the
//! original talks to PipeWire/PulseAudio through `parec` / `pacat` subprocesses
//! (with `arecord` / `aplay` as the ALSA fallback), so 16 kHz mono i16 is done
//! by the sound server. The pure ring buffer is unit-tested; the subprocess
//! layer compiles and is exercised with a fake command in the tests.

#![forbid(unsafe_code)]

pub mod apm;
pub mod buffer;
pub mod capture;
pub mod jitter;
pub mod mixer;
pub mod pipeline;
pub mod processing;
pub mod recorder;
pub mod wsola;

pub use apm::InProcessApm;
pub use buffer::FrameBuffer;
pub use capture::{
    mic_command, speaker_command, CardMic, CardSpeaker, MicCapture, Speaker, CARD_LATENCY_MS,
    FRAME_BYTES,
};
pub use mixer::{playout_step, run_playout, Mixer, PcmSink};
pub use pipeline::{bytes_to_i16, AudioPipeline};
pub use processing::AudioProcessing;
pub use recorder::{ogg_opus_duration_secs, play_ogg_opus, CallRecorder, RecordingSummary};
