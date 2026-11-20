//! Rust port of the Zalo call engine wire format.
//!
//! The reference JavaScript call engine already implements the
//! control plane and media framing against live captures. This crate ports
//! the pure, byte-level layers so the rest of the engine can be rewritten in
//! Rust: SRTP AES-CM (RFC 3711) and the ZRTP packet header used by Zalo's
//! zrtc stack.

#![forbid(unsafe_code)]

pub mod audio;
pub mod call_ui;
pub mod clip_guard;
pub mod engine;
pub mod leveler;
pub mod media;
pub mod p2p;
pub mod rtcp;
pub mod rtp;
pub mod runtime;
pub mod send_rate;
pub mod srtp;
pub mod video;
pub mod zrtc_backend;
pub mod zrtc_helpers;
pub mod zrtp;

pub use rtp::{
    build_audio_rtp_header, read_audio_rtp_header, AudioRtpHeader, AUDIO_PT, FRAME_SAMPLES,
};
pub use srtp::{
    master_from_sess_id, packet_iv, parse_rtp, protect_rtp, session_keys, unprotect_rtp, RtpHeader,
    SessionKeys, SrtpError,
};
pub use zrtp::{
    build_control_header, build_end_call_packet, build_init_zrtp_packet, build_ping_packet,
    unwrap_packet, wrap_media_packet, Command, MsgType, ParsedPacket, CONTROL_HEADER_LEN,
    MAGIC_BYTE, MEDIA_HEADER_LEN,
};
