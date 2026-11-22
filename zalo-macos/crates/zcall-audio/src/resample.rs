//! Rate conversion shared by the CoreAudio host. Pure and tested on every OS.

/// Expand interleaved `i16` PCM to device-rate `f32` in `-1.0..1.0`.
#[must_use]
pub fn resample_to_device(
    pcm: &[i16],
    in_rate: u32,
    in_channels: u8,
    out_rate: u32,
    out_channels: u16,
) -> Vec<f32> {
    let in_channels = usize::from(in_channels.max(1));
    let out_channels = usize::from(out_channels.max(1));
    let in_frames = pcm.len() / in_channels;
    if in_frames == 0 || in_rate == 0 || out_rate == 0 {
        return Vec::new();
    }
    let out_frames =
        ((in_frames as u64) * u64::from(out_rate) / u64::from(in_rate)).max(1) as usize;
    let mut out = Vec::with_capacity(out_frames * out_channels);
    for index in 0..out_frames {
        let src = ((index as u64) * u64::from(in_rate) / u64::from(out_rate)) as usize;
        let src = src.min(in_frames - 1) * in_channels;
        let mut sum = 0_i32;
        for sample in &pcm[src..src + in_channels] {
            sum += i32::from(*sample);
        }
        let mono = (sum / in_channels as i32).clamp(i16::MIN as i32, i16::MAX as i32) as f32
            / 32768.0;
        for _ in 0..out_channels {
            out.push(mono);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resample_16k_mono_to_48k_stereo_triples_and_duplicates() {
        let pcm = vec![0_i16, 16_384, -16_384, 32_767];
        let out = resample_to_device(&pcm, 16_000, 1, 48_000, 2);
        assert_eq!(out.len(), 4 * 3 * 2);
        assert!(out[0].abs() < 0.001);
        assert!((out[6] - 0.5).abs() < 0.02);
    }

    #[test]
    fn resample_keeps_frame_count_when_rates_match() {
        let pcm = vec![1000_i16; 320];
        let out = resample_to_device(&pcm, 16_000, 1, 16_000, 1);
        assert_eq!(out.len(), 320);
    }
}
