//! Shared BGRA → I420 conversion and DirectShow device listing for Windows capture.

use std::process::{Command, Stdio};

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

#[cfg(target_os = "windows")]
pub(crate) const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Applies `CREATE_NO_WINDOW` on Windows so background helper processes never
/// flash a black console window.
pub(crate) fn configure_hidden_command(cmd: &mut Command) -> &mut Command {
    #[cfg(target_os = "windows")]
    {
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Parses `ffmpeg -list_devices true -f dshow -i dummy` stderr output for
/// video capture device names.
///
/// Supports both older ffmpeg (separate `"DirectShow video devices"` section)
/// and newer ffmpeg (`"Name" (video)` inline tag).
#[must_use]
pub(crate) fn parse_dshow_video_devices(stderr: &str) -> Vec<String> {
    let mut devices = Vec::new();
    let mut in_video_section = false;
    for line in stderr.lines() {
        let lower = line.to_ascii_lowercase();
        if lower.contains("directshow video devices") {
            in_video_section = true;
            continue;
        }
        if lower.contains("directshow audio devices") {
            in_video_section = false;
            continue;
        }
        if lower.contains("alternative name") {
            continue;
        }
        let is_video_tagged = lower.contains("(video)");
        if !in_video_section && !is_video_tagged {
            continue;
        }
        if let Some(first) = line.find('"') {
            let rest = &line[first + 1..];
            if let Some(end) = rest.find('"') {
                let name = rest[..end].trim();
                if !name.is_empty() && !devices.iter().any(|d| d == name) {
                    devices.push(name.to_owned());
                }
            }
        }
    }
    devices
}

/// Lists DirectShow video capture devices via `ffmpeg`. Empty if `ffmpeg` is absent.
#[must_use]
pub(crate) fn list_dshow_video_devices() -> Vec<String> {
    if cfg!(test) {
        return Vec::new();
    }
    let mut cmd = Command::new("ffmpeg");
    configure_hidden_command(&mut cmd)
        .args(["-hide_banner", "-list_devices", "true", "-f", "dshow", "-i", "dummy"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let Ok(output) = cmd.output() else {
        return Vec::new();
    };
    let stderr = String::from_utf8_lossy(&output.stderr);
    parse_dshow_video_devices(&stderr)
}

/// `true` when `ffmpeg` is available on `PATH`.
#[must_use]
pub(crate) fn ffmpeg_available() -> bool {
    let mut cmd = Command::new("ffmpeg");
    configure_hidden_command(&mut cmd)
        .arg("-version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    cmd.status().is_ok_and(|status| status.success())
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

/// Nearest-neighbour shrink used for local previews and screen scaling.
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
    fn parses_modern_and_legacy_dshow_video_lists() {
        let modern = "\
[dshow @ 000001] \"Integrated Webcam\" (video)\n\
[dshow @ 000001]   Alternative name \"@device_pnp_\\\\?\\usb#vid\"\n\
[dshow @ 000001] \"Microphone Array\" (audio)\n";
        assert_eq!(parse_dshow_video_devices(modern), vec!["Integrated Webcam"]);

        let legacy = "\
[dshow @ 000001] DirectShow video devices (some may be both video and audio devices)\n\
[dshow @ 000001]  \"HD Pro Webcam C920\"\n\
[dshow @ 000001]     Alternative name \"@device_pnp_123\"\n\
[dshow @ 000001] DirectShow audio devices\n\
[dshow @ 000001]  \"Mic In\"\n";
        assert_eq!(
            parse_dshow_video_devices(legacy),
            vec!["HD Pro Webcam C920"]
        );
    }

    #[test]
    fn i420_length_matches_even_frame() {
        let bgra = vec![255_u8; 4 * 4 * 4];
        let yuv = bgra_to_i420(&bgra, 4, 4);
        assert_eq!(yuv.len(), 4 * 4 + 2 * 2 * 2);
    }
}
