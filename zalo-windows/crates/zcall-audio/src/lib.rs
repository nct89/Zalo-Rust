//! WASAPI capture and playout for the Windows call engine.
//!
//! Off Windows the same API compiles and reports that no device is available,
//! so the crate can sit in the shared cross-platform workspace.

mod resample;

#[cfg(target_os = "windows")]
#[path = "host_wasapi.rs"]
mod host;
#[cfg(not(target_os = "windows"))]
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
