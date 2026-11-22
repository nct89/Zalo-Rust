//! Shared BGRA → I420 conversion and AVFoundation device listing for macOS capture.

use std::process::{Command, Stdio};

/// One AVFoundation video device from `ffmpeg -list_devices`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AvDevice {
    /// Device index passed to `-i {index}:none`.
    pub index: String,
    /// Human-readable name.
    pub name: String,
}

impl AvDevice {
    /// Screen-capture devices, as opposed to cameras.
    #[must_use]
    pub(crate) fn is_screen(&self) -> bool {
        let name = self.name.to_ascii_lowercase();
        name.contains("capture screen")
            || name.contains("screen capture")
            || name.contains("màn hình")
            || name.contains("display")
    }
}

/// Parses ffmpeg's AVFoundation video-device listing.
#[must_use]
pub(crate) fn parse_avfoundation_list(stderr: &str) -> Vec<AvDevice> {
    let mut devices = Vec::new();
    let mut video = false;
    for line in stderr.lines() {
        if line.contains("AVFoundation video devices") {
            video = true;
            continue;
        }
        if line.contains("AVFoundation audio devices") {
            break;
        }
        if !video {
            continue;
        }
        if let Some(device) = parse_device_line(line) {
            devices.push(device);
        }
    }
    devices
}

fn parse_device_line(line: &str) -> Option<AvDevice> {
    let bracket = line.rfind('[')?;
    let rest = &line[bracket + 1..];
    let (num, name) = rest.split_once("] ")?;
    if num.is_empty() || !num.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    Some(AvDevice {
        index: num.to_owned(),
        name: name.trim().to_owned(),
    })
}

/// Runs `ffmpeg -f avfoundation -list_devices`. Empty when ffmpeg is missing.
pub(crate) fn list_video_devices() -> Vec<AvDevice> {
    if cfg!(test) {
        return Vec::new();
    }
    let output = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-f",
            "avfoundation",
            "-list_devices",
            "true",
            "-i",
            "",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    let stderr = String::from_utf8_lossy(&output.stderr);
    parse_avfoundation_list(&stderr)
}

/// `true` when `ffmpeg` can be spawned.
#[must_use]
pub(crate) fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// BT.601 BGRA to packed I420. Width and height must be even.
#[must_use]
pub(crate) fn bgra_to_i420(bgra: &[u8], w: usize, h: usize) -> Vec<u8> {
    let y_len = w.saturating_mul(h);
    let cw = w / 2;
    let ch = h / 2;
    let c_len = cw.saturating_mul(ch);
    let mut out = vec![0_u8; y_len + c_len * 2];
    if w < 2 || h < 2 || bgra.len() < w * h * 4 {
        return out;
    }
    let (y_plane, rest) = out.split_at_mut(y_len);
    let (u_plane, v_plane) = rest.split_at_mut(c_len);
    for cy in 0..ch {
        for cx in 0..cw {
            let mut rs = 0_i32;
            let mut gs = 0_i32;
            let mut bs = 0_i32;
            for dy in 0..2 {
                for dx in 0..2 {
                    let x = cx * 2 + dx;
                    let y = cy * 2 + dy;
                    let p = (y * w + x) * 4;
                    let b = i32::from(bgra[p]);
                    let g = i32::from(bgra[p + 1]);
                    let r = i32::from(bgra[p + 2]);
                    bs += b;
                    gs += g;
                    rs += r;
                    let luma = ((66 * r + 129 * g + 25 * b + 128) >> 8) + 16;
                    y_plane[y * w + x] = luma.clamp(0, 255) as u8;
                }
            }
            let r = rs / 4;
            let g = gs / 4;
            let b = bs / 4;
            let idx = cy * cw + cx;
            u_plane[idx] = (((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128).clamp(0, 255) as u8;
            v_plane[idx] = (((112 * r - 94 * g - 18 * b + 128) >> 8) + 128).clamp(0, 255) as u8;
        }
    }
    out
}

/// Nearest-neighbour shrink used for the local preview.
#[must_use]
pub(crate) fn downscale_bgra(src: &[u8], sw: usize, sh: usize, dw: usize, dh: usize) -> Vec<u8> {
    if sw == 0 || sh == 0 || dw == 0 || dh == 0 || src.len() < sw * sh * 4 {
        return Vec::new();
    }
    if sw == dw && sh == dh {
        return src.to_vec();
    }
    let mut dst = vec![0_u8; dw * dh * 4];
    for y in 0..dh {
        let sy = y * sh / dh;
        for x in 0..dw {
            let sx = x * sw / dw;
            let s = (sy * sw + sx) * 4;
            let d = (y * dw + x) * 4;
            dst[d..d + 4].copy_from_slice(&src[s..s + 4]);
        }
    }
    dst
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_avfoundation_camera_and_screen_lines() {
        let stderr = "\
[AVFoundation indev @ 0x1] AVFoundation video devices:\n\
[AVFoundation indev @ 0x1] [0] FaceTime HD Camera\n\
[AVFoundation indev @ 0x1] [1] Capture screen 0\n\
[AVFoundation indev @ 0x1] AVFoundation audio devices:\n\
[AVFoundation indev @ 0x1] [0] MacBook Pro Microphone\n";
        let devices = parse_avfoundation_list(stderr);
        assert_eq!(devices.len(), 2);
        assert_eq!(devices[0].index, "0");
        assert!(!devices[0].is_screen());
        assert!(devices[1].is_screen());
    }

    #[test]
    fn i420_length_matches_even_frame() {
        let bgra = vec![255_u8; 4 * 4 * 4];
        let yuv = bgra_to_i420(&bgra, 4, 4);
        assert_eq!(yuv.len(), 4 * 4 + 2 * 2 * 2);
    }
}
