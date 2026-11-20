//! Opus encode/decode for Zalo's 16 kHz mono, 20 ms audio frames.
//!
//! Zalo sends `opus/16000/1`, 20 ms frames (320 samples). This wraps the
//! `opus` crate (system libopus) with the exact parameters the call engine
//! uses, so `send_audio_frame` can be fed real PCM.

use opus::{Application, Bandwidth, Bitrate, Channels, Decoder, Encoder, Signal};
use thiserror::Error;

/// Sample rate in Hz.
pub const SAMPLE_RATE: u32 = 16_000;

/// Samples per 20 ms frame.
pub const FRAME_SAMPLES: usize = 320;

/// Why an Opus operation failed.
#[derive(Debug, Error)]
pub enum OpusError {
    /// The underlying libopus call failed.
    #[error("opus error: {0}")]
    Opus(#[from] opus::Error),
}

/// Parameters for configuring [`OpusEncoder`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OpusEncoderConfig {
    /// Target bitrate in bits per second (`audioBitrate * 1000`, default 32_000).
    pub bitrate_bps: i32,
    /// Encoder computational complexity 0..=10 (default 10).
    pub complexity: i32,
    /// Enable in-band FEC (`audioFecInband`, default true).
    pub inband_fec: bool,
    /// Expected packet loss percentage when FEC is enabled (default 5).
    pub packet_loss_perc: i32,
}

impl Default for OpusEncoderConfig {
    fn default() -> Self {
        Self {
            bitrate_bps: 32_000,
            complexity: 10,
            inband_fec: true,
            packet_loss_perc: 5,
        }
    }
}

/// A 16 kHz mono Opus encoder (VoIP mode).
pub struct OpusEncoder {
    inner: Encoder,
}

impl OpusEncoder {
    /// Builds an encoder with 1-1 default settings (32 kbps, complexity 10,
    /// VoIP voice mode, in-band FEC for 5 % packet loss).
    ///
    /// # Errors
    ///
    /// Returns [`OpusError`] when libopus rejects the parameters.
    pub fn new() -> Result<Self, OpusError> {
        Self::with_config(OpusEncoderConfig::default())
    }

    /// Builds an encoder with explicit settings from `zrtcConfig`.
    ///
    /// # Errors
    ///
    /// Returns [`OpusError`] when libopus rejects the parameters.
    pub fn with_config(cfg: OpusEncoderConfig) -> Result<Self, OpusError> {
        let mut inner = Encoder::new(SAMPLE_RATE, Channels::Mono, Application::Voip)?;
        inner.set_signal(Signal::Voice)?;
        inner.set_max_bandwidth(Bandwidth::Wideband)?;
        inner.set_vbr(true)?;
        inner.set_vbr_constraint(true)?;
        inner.set_dtx(false)?;
        inner.set_lsb_depth(16)?;
        let mut enc = Self { inner };
        enc.apply_config(cfg)?;
        Ok(enc)
    }

    /// Updates encoder settings in place without recreating the capture pipeline.
    ///
    /// # Errors
    ///
    /// Returns [`OpusError`] when libopus rejects a parameter.
    pub fn apply_config(&mut self, cfg: OpusEncoderConfig) -> Result<(), OpusError> {
        let bitrate = cfg.bitrate_bps.clamp(6_000, 128_000);
        let complexity = cfg.complexity.clamp(0, 10);
        let loss = if cfg.inband_fec {
            cfg.packet_loss_perc.clamp(0, 100)
        } else {
            0
        };
        self.inner.set_bitrate(Bitrate::Bits(bitrate))?;
        self.inner.set_complexity(complexity)?;
        self.inner.set_inband_fec(cfg.inband_fec)?;
        self.inner.set_packet_loss_perc(loss)?;
        Ok(())
    }

    /// Updates expected uplink packet loss percentage (for in-band LBRR FEC).
    ///
    /// # Errors
    ///
    /// Returns [`OpusError`] when libopus rejects the value.
    pub fn set_packet_loss_perc(&mut self, loss_perc: i32) -> Result<(), OpusError> {
        self.inner.set_packet_loss_perc(loss_perc.clamp(0, 100))?;
        Ok(())
    }

    /// Encodes one 320-sample PCM frame into `out`, returning the byte count.
    ///
    /// # Errors
    ///
    /// Returns [`OpusError`] when encoding fails.
    pub fn encode(&mut self, pcm: &[i16], out: &mut [u8]) -> Result<usize, OpusError> {
        Ok(self.inner.encode(pcm, out)?)
    }

    /// Encodes one frame into a fresh vector.
    ///
    /// # Errors
    ///
    /// Returns [`OpusError`] when encoding fails.
    pub fn encode_frame(&mut self, pcm: &[i16]) -> Result<Vec<u8>, OpusError> {
        let mut out = vec![0_u8; 1500];
        let n = self.encode(pcm, &mut out)?;
        out.truncate(n);
        Ok(out)
    }
}

/// A 16 kHz mono Opus decoder.
pub struct OpusDecoder {
    inner: Decoder,
}

impl OpusDecoder {
    /// Builds a decoder.
    ///
    /// # Errors
    ///
    /// Returns [`OpusError`] when libopus rejects the parameters.
    pub fn new() -> Result<Self, OpusError> {
        Ok(Self {
            inner: Decoder::new(SAMPLE_RATE, Channels::Mono)?,
        })
    }

    /// Decodes one packet into PCM, returning the sample count.
    ///
    /// # Errors
    ///
    /// Returns [`OpusError`] when decoding fails.
    pub fn decode(&mut self, data: &[u8], pcm: &mut [i16]) -> Result<usize, OpusError> {
        self.decode_with_fec(data, false, pcm)
    }

    /// Decodes with optional in-band forward error correction.
    ///
    /// # Errors
    ///
    /// Returns [`OpusError`] when decoding fails.
    pub fn decode_with_fec(
        &mut self,
        data: &[u8],
        fec: bool,
        pcm: &mut [i16],
    ) -> Result<usize, OpusError> {
        Ok(self.inner.decode(data, pcm, fec)?)
    }

    /// Decodes a lost frame with packet loss concealment.
    ///
    /// # Errors
    ///
    /// Returns [`OpusError`] when decoding fails.
    pub fn decode_loss(&mut self, pcm: &mut [i16]) -> Result<usize, OpusError> {
        self.decode_with_fec(&[], false, pcm)
    }

    /// Decodes one packet into a fresh vector.
    ///
    /// # Errors
    ///
    /// Returns [`OpusError`] when decoding fails.
    pub fn decode_frame(&mut self, data: &[u8]) -> Result<Vec<i16>, OpusError> {
        let mut pcm = vec![0_i16; FRAME_SAMPLES * 2];
        let n = self.decode(data, &mut pcm)?;
        pcm.truncate(n);
        Ok(pcm)
    }
}

/// A 16 kHz stereo Opus encoder for dual-channel call recording
/// (Left = local microphone, Right = remote/group mix).
pub struct OpusStereoEncoder {
    inner: Encoder,
}

impl OpusStereoEncoder {
    /// Builds a 16 kHz stereo encoder forced to 2 channels so one-sided speech
    /// stays cleanly separated on its channel instead of collapsing to mono.
    ///
    /// # Errors
    ///
    /// Returns [`OpusError`] when libopus rejects the parameters.
    pub fn new(bitrate_bps: i32) -> Result<Self, OpusError> {
        let mut inner = Encoder::new(SAMPLE_RATE, Channels::Stereo, Application::Voip)?;
        inner.set_signal(Signal::Voice)?;
        inner.set_max_bandwidth(Bandwidth::Wideband)?;
        inner.set_force_channels(Some(Channels::Stereo))?;
        inner.set_bitrate(Bitrate::Bits(bitrate_bps.clamp(16_000, 128_000)))?;
        inner.set_complexity(6)?;
        inner.set_vbr(true)?;
        inner.set_vbr_constraint(true)?;
        inner.set_dtx(false)?;
        inner.set_lsb_depth(16)?;
        Ok(Self { inner })
    }

    /// Encoder algorithmic delay scaled to the 48 kHz Ogg Opus clock (RFC 7845 `Pre-skip`).
    pub fn pre_skip_48k(&mut self) -> u16 {
        let lookahead = self.inner.get_lookahead().unwrap_or(104).max(0) as u32;
        let scaled = lookahead.saturating_mul(48_000 / SAMPLE_RATE);
        scaled.min(u32::from(u16::MAX)) as u16
    }

    /// Encodes one interleaved stereo 20 ms frame (`FRAME_SAMPLES * 2 = 640` samples).
    ///
    /// # Errors
    ///
    /// Returns [`OpusError`] when encoding fails.
    pub fn encode_frame(&mut self, interleaved_pcm: &[i16]) -> Result<Vec<u8>, OpusError> {
        let mut out = vec![0_u8; 1500];
        let n = self.inner.encode(interleaved_pcm, &mut out)?;
        out.truncate(n);
        Ok(out)
    }
}

/// A 16 kHz stereo Opus decoder (used to verify stereo call recordings).
pub struct OpusStereoDecoder {
    inner: Decoder,
}

impl OpusStereoDecoder {
    /// Builds a 16 kHz stereo decoder.
    ///
    /// # Errors
    ///
    /// Returns [`OpusError`] when libopus rejects the parameters.
    pub fn new() -> Result<Self, OpusError> {
        Ok(Self {
            inner: Decoder::new(SAMPLE_RATE, Channels::Stereo)?,
        })
    }

    /// Decodes one stereo packet into interleaved `[L0, R0, L1, R1, ...]` PCM.
    ///
    /// # Errors
    ///
    /// Returns [`OpusError`] when decoding fails.
    pub fn decode_frame(&mut self, data: &[u8]) -> Result<Vec<i16>, OpusError> {
        let mut pcm = vec![0_i16; FRAME_SAMPLES * 4];
        let per_channel = self.inner.decode(data, &mut pcm, false)?;
        pcm.truncate(per_channel * 2);
        Ok(pcm)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine_440() -> Vec<i16> {
        (0..FRAME_SAMPLES)
            .map(|i| {
                let t = i as f64 / SAMPLE_RATE as f64;
                (10_000.0 * (2.0 * std::f64::consts::PI * 440.0 * t).sin()) as i16
            })
            .collect()
    }

    #[test]
    fn round_trips_a_tone() {
        let pcm = sine_440();
        let mut encoder = OpusEncoder::new().expect("encoder");
        let mut decoder = OpusDecoder::new().expect("decoder");
        let encoded = encoder.encode_frame(&pcm).expect("encode");
        assert!(!encoded.is_empty());
        let decoded = decoder.decode_frame(&encoded).expect("decode");
        assert_eq!(decoded.len(), FRAME_SAMPLES);
        let peak = decoded.iter().map(|s| s.abs()).max().unwrap_or(0);
        assert!(peak > 1_000, "decoded tone should be audible, peak {peak}");
    }

    #[test]
    fn round_trips_silence() {
        let silence = vec![0_i16; FRAME_SAMPLES];
        let mut encoder = OpusEncoder::new().expect("encoder");
        let mut decoder = OpusDecoder::new().expect("decoder");
        let encoded = encoder.encode_frame(&silence).expect("encode");
        let decoded = decoder.decode_frame(&encoded).expect("decode");
        assert_eq!(decoded.len(), FRAME_SAMPLES);
        let peak = decoded.iter().map(|s| s.abs()).max().unwrap_or(0);
        assert!(peak < 50, "silence should stay silent, peak {peak}");
    }

    #[test]
    fn applies_custom_encoder_config() {
        let cfg = OpusEncoderConfig {
            bitrate_bps: 20_000,
            complexity: 6,
            inband_fec: false,
            packet_loss_perc: 0,
        };
        let mut encoder = OpusEncoder::with_config(cfg).expect("encoder");
        let encoded = encoder.encode_frame(&sine_440()).expect("encode");
        assert!(!encoded.is_empty());
        encoder
            .apply_config(OpusEncoderConfig::default())
            .expect("reconfigure");
    }
}
