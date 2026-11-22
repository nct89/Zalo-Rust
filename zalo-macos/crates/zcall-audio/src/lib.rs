//! CoreAudio capture and playout for the macOS call engine.
//!
//! Off macOS the same API compiles and reports that no device is available, so
//! the crate can sit in the shared workspace.

mod resample;

#[cfg(target_os = "macos")]
#[path = "host_coreaudio.rs"]
mod host;
#[cfg(not(target_os = "macos"))]
#[path = "host_stub.rs"]
mod host;

pub use resample::resample_to_device;

/// Namespaced the same way as the Linux `zcall-audio` crate (`zcall_audio::capture`).
pub mod capture {
    pub use crate::host::{
        CardMic, CardSpeaker, MicCapture, Speaker, CARD_LATENCY_MS, CARD_MIC_LATENCY_MS,
        FRAME_BYTES, FRAME_SAMPLES,
    };
}

pub use host::{
    list_endpoints, CardMic, CardSpeaker, MicCapture, Speaker, CARD_LATENCY_MS,
    CARD_MIC_LATENCY_MS, FRAME_BYTES, FRAME_SAMPLES,
};
