//! Rate conversion shared by the WASAPI host. Pure and tested on every OS.
//!
//! Uses [`zcall_dsp::resampler::PolyphaseResampler`] (Blackman-Harris windowed
//! sinc FIR, > 60 dB stopband attenuation) instead of zero-order hold so
//! 16 ↔ 48 kHz and 16 ↔ 44.1 kHz conversions never introduce aliasing or
//! staircase hiss.

use zcall_dsp::resampler::PolyphaseResampler;

/// Expand interleaved `i16` PCM to device-rate `f32` in `-1.0..1.0` using a
/// linear-phase polyphase FIR resampler.
#[must_use]
pub fn resample_to_device(
    pcm: &[i16],
    in_rate: u32,
    in_channels: u8,
    out_rate: u32,
    out_channels: u16,
) -> Vec<f32> {
    if pcm.is_empty() || in_rate == 0 || out_rate == 0 {
        return Vec::new();
    }
    let mut resampler = PolyphaseResampler::new(in_rate, out_rate);
    resampler.upmix_i16_to_f32(pcm, in_channels, out_channels)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resample_16k_mono_to_48k_stereo_triples_and_duplicates() {
        let pcm = vec![16_384_i16; 64];
        let out = resample_to_device(&pcm, 16_000, 1, 48_000, 2);
        assert_eq!(out.len(), 64 * 3 * 2);
        // After FIR settle (past half-filter length), DC level is preserved cleanly.
        let tail = &out[out.len() - 20..];
        for pair in tail.chunks_exact(2) {
            assert!((pair[0] - 0.5).abs() < 0.02, "left={}", pair[0]);
            assert!((pair[0] - pair[1]).abs() < 1e-6);
        }
    }

    #[test]
    fn resample_keeps_frame_count_when_rates_match() {
        let pcm = vec![1000_i16; 320];
        let out = resample_to_device(&pcm, 16_000, 1, 16_000, 1);
        assert_eq!(out.len(), 320);
    }
}
