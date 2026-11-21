//! Remote H.264 for the call window.
//!
//! Electron decodes Annex-B with WebCodecs (`plugins/zcall/ui/call.js`).
//! This uses the same access units from `VideoAssembler` and OpenH264's
//! software decoder, one decoder per sender SSRC so a group call does not
//! smear two people through one decoder.

use std::collections::BTreeMap;
use std::sync::Arc;

use openh264::decoder::Decoder;
use openh264::formats::YUVSource;
use zcall_core::video::VideoFrame;

/// Longest edge uploaded to the call window. Fits inside GPUI's 1024 px atlas
/// while keeping 720p/1080p phone frames crisp (`960×540` / `540×960`).
pub(crate) const PREVIEW_MAX_EDGE: u32 = 960;

/// One decoded picture, BGRA for `RenderImage`.
#[derive(Clone, Debug)]
pub(crate) struct RemotePicture {
    /// Width in pixels.
    pub w: u32,
    /// Height in pixels.
    pub h: u32,
    /// Tightly packed BGRA.
    pub bgra: Arc<Vec<u8>>,
}

/// What `push` wants the call worker to do.
pub(crate) enum Shown {
    /// A picture ready to paint. `src` is the sender SSRC.
    Picture {
        /// Sender SSRC.
        src: u32,
        /// Decoded frame.
        picture: RemotePicture,
    },
    /// This sender needs a key frame before more deltas will decode.
    NeedKey(u32),
}

/// Accepts both landscape (up to 1920×1200) and portrait (e.g. 720×1280,
/// 1080×1920) mobile video frames while rejecting corrupted dimensions.
pub(crate) fn supported_picture_size(w: usize, h: usize) -> bool {
    w > 0 && h > 0 && w.max(h) <= 1920 && w.min(h) <= 1200
}

/// Decoders keyed by SSRC. 1-1 uses the single peer SSRC.
pub(crate) struct RemoteVideo {
    decoders: BTreeMap<u32, Decoder>,
}

impl RemoteVideo {
    /// No decoders yet. The first key frame of each sender creates one.
    pub(crate) fn new() -> Self {
        Self {
            decoders: BTreeMap::new(),
        }
    }

    /// Decodes one access unit. Deltas before the first key ask for a PLI.
    pub(crate) fn push(&mut self, frame: &VideoFrame) -> Option<Shown> {
        let src = frame.src;
        if !self.decoders.contains_key(&src) {
            if !frame.key {
                return Some(Shown::NeedKey(src));
            }
            match Decoder::new() {
                Ok(decoder) => {
                    self.decoders.insert(src, decoder);
                }
                Err(_) => return Some(Shown::NeedKey(src)),
            }
        }
        let Some(decoder) = self.decoders.get_mut(&src) else {
            return Some(Shown::NeedKey(src));
        };
        let decoded = match decoder.decode(&frame.data) {
            Ok(decoded) => decoded,
            Err(_) => {
                self.decoders.remove(&src);
                return Some(Shown::NeedKey(src));
            }
        };
        let Some(yuv) = decoded else {
            return None;
        };
        let (w, h) = yuv.dimensions();
        if !supported_picture_size(w, h) {
            return None;
        }
        let bgra = yuv_to_bgra(&yuv, w, h);
        Some(Shown::Picture {
            src,
            picture: RemotePicture {
                w: w as u32,
                h: h as u32,
                bgra: Arc::new(bgra),
            },
        })
    }
}

/// `(width, height)` of the texture the call window should upload.
pub(crate) fn fit_preview(w: u32, h: u32) -> (u32, u32) {
    let long = w.max(h);
    if long <= PREVIEW_MAX_EDGE || long == 0 {
        return (w, h);
    }
    let scale = PREVIEW_MAX_EDGE as f32 / long as f32;
    let nw = ((w as f32 * scale).round() as u32).max(1);
    let nh = ((h as f32 * scale).round() as u32).max(1);
    (nw, nh)
}

/// Packed BGRA for the call window, scaled down when the frame is larger than
/// [`PREVIEW_MAX_EDGE`].
pub(crate) fn preview_bgra(picture: &RemotePicture) -> Option<(u32, u32, Vec<u8>)> {
    let w = picture.w;
    let h = picture.h;
    if w == 0 || h == 0 || picture.bgra.len() != (w as usize) * (h as usize) * 4 {
        return None;
    }
    let (nw, nh) = fit_preview(w, h);
    if (nw, nh) == (w, h) {
        return Some((w, h, picture.bgra.as_ref().clone()));
    }
    Some((
        nw,
        nh,
        scale_bgra_bilinear(picture.bgra.as_ref(), w, h, nw, nh),
    ))
}

pub(crate) fn scale_bgra_bilinear(src: &[u8], sw: u32, sh: u32, dw: u32, dh: u32) -> Vec<u8> {
    let mut dst = vec![0_u8; (dw as usize) * (dh as usize) * 4];
    if sw == 0 || sh == 0 || dw == 0 || dh == 0 {
        return dst;
    }
    let sw_u = sw as usize;
    let sh_u = sh as usize;
    let dw_u = dw as usize;
    let dh_u = dh as usize;
    let max_x = (sw_u - 1) << 16;
    let max_y = (sh_u - 1) << 16;
    let step_x = if dw_u > 1 {
        ((sw_u - 1) << 16) / (dw_u - 1)
    } else {
        0
    };
    let step_y = if dh_u > 1 {
        ((sh_u - 1) << 16) / (dh_u - 1)
    } else {
        0
    };
    let mut xs = Vec::with_capacity(dw_u);
    for x in 0..dw_u {
        let fx = (x * step_x).min(max_x);
        let x0 = fx >> 16;
        let x1 = (x0 + 1).min(sw_u - 1);
        let wx = ((fx & 0xffff) >> 8) as u32;
        xs.push((x0 * 4, x1 * 4, wx));
    }
    for y in 0..dh_u {
        let fy = (y * step_y).min(max_y);
        let y0 = fy >> 16;
        let y1 = (y0 + 1).min(sh_u - 1);
        let wy = ((fy & 0xffff) >> 8) as u32;
        let iwy = 256 - wy;
        let r0 = y0 * sw_u * 4;
        let r1 = y1 * sw_u * 4;
        let dst_row = y * dw_u * 4;
        for (x, &(x0, x1, wx)) in xs.iter().enumerate() {
            let iwx = 256 - wx;
            let p00 = r0 + x0;
            let p01 = r0 + x1;
            let p10 = r1 + x0;
            let p11 = r1 + x1;
            let d = dst_row + x * 4;
            for c in 0..4 {
                let top = u32::from(src[p00 + c]) * iwx + u32::from(src[p01 + c]) * wx;
                let bot = u32::from(src[p10 + c]) * iwx + u32::from(src[p11 + c]) * wx;
                dst[d + c] = ((top * iwy + bot * wy + 32768) >> 16) as u8;
            }
        }
    }
    dst
}

/// Converts OpenH264 YUV420p planes directly into packed BGRA `[B, G, R, 255]`
/// matching GPUI's `RenderImage` pixel layout without an intermediate RGB copy.
pub(crate) fn yuv_to_bgra(yuv: &impl YUVSource, w: usize, h: usize) -> Vec<u8> {
    let (y_stride, u_stride, v_stride) = yuv.strides();
    let y_plane = yuv.y();
    let u_plane = yuv.u();
    let v_plane = yuv.v();
    let mut bgra = vec![255_u8; w * h * 4];
    let y_max = y_plane.len();
    let u_max = u_plane.len();
    let v_max = v_plane.len();

    for row in 0..h {
        let y_row = row * y_stride;
        let u_row = (row >> 1) * u_stride;
        let v_row = (row >> 1) * v_stride;
        let dst_row = row * w * 4;
        if y_row + w > y_max {
            break;
        }
        for col in 0..w {
            let c_idx = col >> 1;
            let u_idx = u_row + c_idx;
            let v_idx = v_row + c_idx;
            if u_idx >= u_max || v_idx >= v_max {
                break;
            }
            let c = i32::from(y_plane[y_row + col]) - 16;
            let d = i32::from(u_plane[u_idx]) - 128;
            let e = i32::from(v_plane[v_idx]) - 128;
            let y298 = 298 * c + 128;
            let r = ((y298 + 409 * e) >> 8).clamp(0, 255) as u8;
            let g = ((y298 - 100 * d - 208 * e) >> 8).clamp(0, 255) as u8;
            let b = ((y298 + 516 * d) >> 8).clamp(0, 255) as u8;
            let px = dst_row + col * 4;
            bgra[px] = b;
            bgra[px + 1] = g;
            bgra[px + 2] = r;
        }
    }
    bgra
}

#[cfg(test)]
mod tests {
    use super::*;
    use openh264::formats::YUVBuffer;

    #[test]
    fn yuv_to_bgra_round_trips_bt601_to_bgra() {
        let quadrants = [
            [0_u8, 0, 0],
            [255_u8, 255, 255],
            [220_u8, 30, 40],
            [25_u8, 80, 230],
        ];
        let mut rgb_in = vec![0_u8; 4 * 4 * 3];
        for y in 0..4 {
            for x in 0..4 {
                let c = quadrants[(y / 2) * 2 + (x / 2)];
                let idx = (y * 4 + x) * 3;
                rgb_in[idx..idx + 3].copy_from_slice(&c);
            }
        }
        let yuv = YUVBuffer::from_rgb_source(openh264::formats::RgbSliceU8::new(&rgb_in, (4, 4)));
        let (w, h) = yuv.dimensions();
        let fast_bgra = yuv_to_bgra(&yuv, w, h);
        assert_eq!(fast_bgra.len(), w * h * 4);
        for (px_rgb, px_bgra) in rgb_in.chunks_exact(3).zip(fast_bgra.chunks_exact(4)) {
            assert_eq!(px_bgra[3], 255);
            assert!((i16::from(px_bgra[0]) - i16::from(px_rgb[2])).abs() <= 4);
            assert!((i16::from(px_bgra[1]) - i16::from(px_rgb[1])).abs() <= 4);
            assert!((i16::from(px_bgra[2]) - i16::from(px_rgb[0])).abs() <= 4);
        }
    }

    #[test]
    fn preview_shrinks_hd_and_keeps_a_corner_pixel() {
        assert_eq!(fit_preview(1280, 720), (960, 540));
        assert_eq!(fit_preview(720, 1280), (540, 960));
        assert_eq!(fit_preview(960, 540), (960, 540));
        assert_eq!(fit_preview(640, 360), (640, 360));
        let mut bgra = vec![0_u8; 8 * 4 * 4];
        bgra[0..4].copy_from_slice(&[1, 2, 3, 255]);
        let picture = RemotePicture {
            w: 8,
            h: 4,
            bgra: Arc::new(bgra),
        };
        let (w, h, bytes) = preview_bgra(&picture).unwrap();
        assert_eq!((w, h), (8, 4));
        assert_eq!(&bytes[0..4], &[1, 2, 3, 255]);
        let big = RemotePicture {
            w: 1280,
            h: 720,
            bgra: Arc::new(vec![9_u8; 1280 * 720 * 4]),
        };
        let (w, h, bytes) = preview_bgra(&big).unwrap();
        assert_eq!((w, h), (960, 540));
        assert_eq!(bytes.len(), 960 * 540 * 4);
        assert_eq!(&bytes[0..4], &[9, 9, 9, 9]);
    }

    #[test]
    fn bilinear_scale_blends_smooth_gradient() {
        // 2x1 black-to-white horizontal gradient scaled to 3x1 should put ~128 in the middle.
        let src = vec![0, 0, 0, 255, 254, 254, 254, 255];
        let dst = scale_bgra_bilinear(&src, 2, 1, 3, 1);
        assert_eq!(&dst[0..4], &[0, 0, 0, 255]);
        assert!((i16::from(dst[4]) - 127).abs() <= 1);
        assert_eq!(dst[7], 255);
        assert_eq!(&dst[8..12], &[254, 254, 254, 255]);
    }

    #[test]
    fn supported_picture_size_allows_portrait_hd_and_fhd() {
        assert!(supported_picture_size(480, 852));
        assert!(supported_picture_size(720, 1280));
        assert!(supported_picture_size(1080, 1920));
        assert!(supported_picture_size(1920, 1080));
        assert!(!supported_picture_size(0, 720));
        assert!(!supported_picture_size(720, 0));
        assert!(!supported_picture_size(2560, 1440));
        assert!(!supported_picture_size(1440, 2560));
    }
}
