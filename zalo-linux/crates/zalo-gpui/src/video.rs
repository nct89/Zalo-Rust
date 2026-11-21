//! Native in-process video probe & poster generator (`mp4` + `openh264`).
//!
//! Reads MP4 duration, dimensions, and decodes the first H.264 keyframe into a
//! JPEG poster entirely in-process without `ffprobe` or `ffmpeg`.

use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

use openh264::decoder::Decoder;
use openh264::formats::YUVSource;

/// Still metadata plus an optional poster file.
#[derive(Clone, Debug, Default)]
pub struct ProbedVideo {
    /// Length in milliseconds. `1` when the container cannot be parsed.
    pub duration_ms: u64,
    /// Pixel width. `0` when unknown.
    pub width: u32,
    /// Pixel height. `0` when unknown.
    pub height: u32,
    /// JPEG written beside the cache. `None` when no H.264 poster could be decoded.
    pub thumb: Option<PathBuf>,
}

/// True for extensions the file picker should send as `chat.video.msg`.
#[must_use]
pub fn is_video_path(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref(),
        Some("mp4" | "mov" | "m4v" | "mkv" | "webm" | "avi" | "3gp")
    )
}

/// Native MP4 header probe + H.264 first-frame poster extraction (no `ffprobe`/`ffmpeg`).
#[must_use]
pub fn inspect(path: &Path) -> ProbedVideo {
    let (duration_ms, width, height) = probe_header(path);
    let thumb = poster(path);
    ProbedVideo {
        duration_ms: if duration_ms == 0 { 1 } else { duration_ms },
        width,
        height,
        thumb,
    }
}

/// Converts a length-prefixed MP4 AVCC sample (4-byte big-endian NAL lengths)
/// into an Annex-B byte stream (`00 00 00 01 [NAL]...`). Optionally prepends
/// SPS and PPS parameter sets before keyframes.
pub(crate) fn avcc_to_annex_b(
    sample: &[u8],
    sps: Option<&[u8]>,
    pps: Option<&[u8]>,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(
        sample.len() + sps.map_or(0, |s| s.len() + 4) + pps.map_or(0, |p| p.len() + 4) + 16,
    );
    if let Some(sps) = sps.filter(|s| !s.is_empty()) {
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(sps);
    }
    if let Some(pps) = pps.filter(|p| !p.is_empty()) {
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(pps);
    }
    let mut pos = 0;
    while pos + 4 <= sample.len() {
        let len = u32::from_be_bytes([
            sample[pos],
            sample[pos + 1],
            sample[pos + 2],
            sample[pos + 3],
        ]) as usize;
        pos += 4;
        if len == 0 || pos.saturating_add(len) > sample.len() {
            break;
        }
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(&sample[pos..pos + len]);
        pos += len;
    }
    out
}

/// Opens an MP4 file and returns `(reader, video_track_id, sps, pps, timescale, sample_count)`.
pub(crate) fn open_h264_mp4(
    path: &Path,
) -> Option<(
    mp4::Mp4Reader<BufReader<File>>,
    u32,
    Vec<u8>,
    Vec<u8>,
    u32,
    u32,
)> {
    let file = File::open(path).ok()?;
    let size = file.metadata().ok()?.len();
    let reader = mp4::Mp4Reader::read_header(BufReader::new(file), size).ok()?;
    let mut found = None;
    for (&track_id, track) in reader.tracks() {
        if track.track_type().ok() == Some(mp4::TrackType::Video)
            && track.media_type().ok() == Some(mp4::MediaType::H264)
        {
            let sps = track.sequence_parameter_set().ok()?.to_vec();
            let pps = track.picture_parameter_set().ok()?.to_vec();
            let timescale = track.timescale().max(1);
            let count = track.sample_count();
            found = Some((track_id, sps, pps, timescale, count));
            break;
        }
    }
    let (track_id, sps, pps, timescale, count) = found?;
    Some((reader, track_id, sps, pps, timescale, count))
}

fn probe_header(path: &Path) -> (u64, u32, u32) {
    let Ok(file) = File::open(path) else {
        return (0, 0, 0);
    };
    let Ok(size) = file.metadata().map(|m| m.len()) else {
        return (0, 0, 0);
    };
    let Ok(reader) = mp4::Mp4Reader::read_header(BufReader::new(file), size) else {
        return (0, 0, 0);
    };
    let duration_ms = reader.duration().as_millis().min(u64::MAX as u128) as u64;
    for track in reader.tracks().values() {
        if track.track_type().ok() == Some(mp4::TrackType::Video) {
            let track_ms = track.duration().as_millis().min(u64::MAX as u128) as u64;
            let ms = if track_ms > 0 { track_ms } else { duration_ms };
            return (ms, u32::from(track.width()), u32::from(track.height()));
        }
    }
    (duration_ms, 0, 0)
}

fn poster(path: &Path) -> Option<PathBuf> {
    let (mut reader, track_id, sps, pps, _, sample_count) = open_h264_mp4(path)?;
    let mut decoder = Decoder::new().ok()?;
    let mut decoded_frame: Option<(u32, u32, Vec<u8>)> = None;
    let max_probe = sample_count.min(60);
    for idx in 1..=max_probe {
        let Ok(Some(sample)) = reader.read_sample(track_id, idx) else {
            break;
        };
        let annex_b = if idx == 1 || sample.is_sync {
            avcc_to_annex_b(&sample.bytes, Some(&sps), Some(&pps))
        } else {
            avcc_to_annex_b(&sample.bytes, None, None)
        };
        if let Ok(Some(yuv)) = decoder.decode(&annex_b) {
            let (w, h) = yuv.dimensions();
            if w > 0 && h > 0 {
                let bgra = crate::call_video::yuv_to_bgra(&yuv, w, h);
                decoded_frame = Some((w as u32, h as u32, bgra));
                break;
            }
        }
    }
    let (w, h, bgra) = decoded_frame?;
    let (tw, th) = if w > 480 {
        let nh = ((u64::from(h) * 480) / u64::from(w)).max(2) as u32;
        (480, nh)
    } else {
        (w, h)
    };
    let scaled = if (tw, th) == (w, h) {
        bgra
    } else {
        crate::call_video::scale_bgra_bilinear(&bgra, w, h, tw, th)
    };
    // Convert BGRA -> RGB for JPEG encoding.
    let mut rgb = Vec::with_capacity((tw as usize) * (th as usize) * 3);
    for px in scaled.chunks_exact(4) {
        rgb.push(px[2]);
        rgb.push(px[1]);
        rgb.push(px[0]);
    }
    let img = image::RgbImage::from_raw(tw, th, rgb)?;
    let dir = crate::media::media_root()
        .map(|root| root.join("photos"))
        .unwrap_or_else(std::env::temp_dir);
    std::fs::create_dir_all(&dir).ok()?;
    let name = format!(
        "video-thumb-{}",
        crate::media::url_key(&path.display().to_string())
    );
    let dest = dir.join(format!("{name}.jpg"));
    let mut out = std::fs::File::create(&dest).ok()?;
    let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 82);
    enc.encode_image(&img).ok()?;
    (dest.is_file() && std::fs::metadata(&dest).is_ok_and(|m| m.len() > 0)).then_some(dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_extensions() {
        assert!(is_video_path(Path::new("/tmp/a.MP4")));
        assert!(is_video_path(Path::new("/tmp/a.webm")));
        assert!(!is_video_path(Path::new("/tmp/a.pdf")));
        assert!(!is_video_path(Path::new("/tmp/a")));
    }

    #[test]
    fn avcc_to_annex_b_prepends_parameter_sets_and_converts_nal_lengths() {
        let sps = [0x67, 0x42, 0x00, 0x1e];
        let pps = [0x68, 0xce, 0x3c, 0x80];
        let mut sample = Vec::new();
        sample.extend_from_slice(&3_u32.to_be_bytes());
        sample.extend_from_slice(&[0x65, 0x88, 0x84]);
        sample.extend_from_slice(&2_u32.to_be_bytes());
        sample.extend_from_slice(&[0x06, 0x05]);

        let annex = avcc_to_annex_b(&sample, Some(&sps), Some(&pps));
        assert_eq!(
            annex,
            vec![
                0, 0, 0, 1, 0x67, 0x42, 0x00, 0x1e,
                0, 0, 0, 1, 0x68, 0xce, 0x3c, 0x80,
                0, 0, 0, 1, 0x65, 0x88, 0x84,
                0, 0, 0, 1, 0x06, 0x05,
            ]
        );
    }
}
