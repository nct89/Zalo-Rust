//! Cross-platform pure-Rust audio DSP for Zalo voice and video calls.
//!
//! Provides the shared signal-processing chain used uniformly across Linux,
//! macOS, and Windows:
//! - [`PolyphaseResampler`]: phase-continuous windowed-sinc FIR rate conversion
//!   between hardware sample rates (48 kHz / 44.1 kHz / 32 kHz) and Opus 16 kHz.
//! - [`InProcessApm`]: in-process WebRTC AudioProcessing (`sonora` AEC3,
//!   leveled WebRTC NS, smooth AGC2 + limiter, and opt-in native 48 kHz RNNoise).
//! - [`JitterBuffer`], [`Mixer`], and [`wsola`]: NetEq-style playout jitter
//!   buffer with Opus FEC/PLC, normalized-correlation WSOLA acceleration, and
//!   click-free crossfades.

#![forbid(unsafe_code)]

pub mod apm;
pub mod jitter;
pub mod mixer;
pub mod resampler;
pub mod wsola;

pub use apm::InProcessApm;
pub use jitter::JitterBuffer;
pub use mixer::{playout_step, run_playout, Mixer, PcmSink};
pub use resampler::PolyphaseResampler;
