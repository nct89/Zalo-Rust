//! Image preparation for the detector and the recognizer.
//!
//! The detector pads each side up to a multiple of 32. Detection tensors use
//! ImageNet mean and standard deviation. Recognizer tensors use `(x - 0.5) / 0.5`.
//! Both are NCHW, one image, three channels.

use image::imageops::{self, FilterType};
use image::{Rgb, RgbImage};

use crate::geometry::{distance, project, Quad};

/// Per-channel normalization of a classifier or recognizer batch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Normalization {
    /// `(x - 0.5) / 0.5`.
    Half,
    /// `(x - mean) / std` with the ImageNet mean and std.
    ImageNet,
}

fn normalize(x: f32, channel: usize, normalization: Normalization) -> f32 {
    const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
    const STD: [f32; 3] = [0.229, 0.224, 0.225];
    match normalization {
        Normalization::Half => (x - 0.5) / 0.5,
        Normalization::ImageNet => (x - MEAN[channel]) / STD[channel],
    }
}

/// Rectifies the quad `q` of `image` into an upright crop (bilinear).
///
/// A crop at least 1.5 times taller than wide is a vertical line; it is
/// rotated 270° and the returned quad starts from the new reading corner.
/// `None` when the quad is degenerate or the image is empty.
pub fn crop(image: &RgbImage, q: Quad) -> Option<(RgbImage, Quad)> {
    if image.width() == 0 || image.height() == 0 {
        return None;
    }
    let width = distance(q[0], q[1])
        .max(distance(q[3], q[2]))
        .round()
        .clamp(2.0, 4096.0) as u32;
    let height = distance(q[0], q[3])
        .max(distance(q[1], q[2]))
        .round()
        .clamp(2.0, 4096.0) as u32;
    let mut out = RgbImage::new(width, height);
    let max_x = f64::from(image.width() - 1);
    let max_y = f64::from(image.height() - 1);
    for y in 0..height {
        for x in 0..width {
            let p = project(
                q,
                f64::from(x) / f64::from(width - 1),
                f64::from(y) / f64::from(height - 1),
            )?;
            let sx = p[0].clamp(0.0, max_x);
            let sy = p[1].clamp(0.0, max_y);
            let x0 = sx.floor() as u32;
            let y0 = sy.floor() as u32;
            let x1 = (x0 + 1).min(image.width() - 1);
            let y1 = (y0 + 1).min(image.height() - 1);
            let fx = sx - f64::from(x0);
            let fy = sy - f64::from(y0);
            let pixel = std::array::from_fn(|c| {
                let top = f64::from(image.get_pixel(x0, y0)[c]) * (1.0 - fx)
                    + f64::from(image.get_pixel(x1, y0)[c]) * fx;
                let bottom = f64::from(image.get_pixel(x0, y1)[c]) * (1.0 - fx)
                    + f64::from(image.get_pixel(x1, y1)[c]) * fx;
                (top * (1.0 - fy) + bottom * fy).round() as u8
            });
            out.put_pixel(x, y, Rgb(pixel));
        }
    }
    if f64::from(height) >= f64::from(width) * 1.5 {
        Some((imageops::rotate270(&out), [q[1], q[2], q[3], q[0]]))
    } else {
        Some((out, q))
    }
}

/// How crops become one NCHW batch.
#[derive(Clone, Copy, Debug)]
pub struct BatchSpec {
    /// Target height.
    pub height: u32,
    /// Fixed model width, or `None` to pad to the widest item (multiple of 8).
    pub fixed_width: Option<u32>,
    /// One luma channel instead of RGB.
    pub grayscale: bool,
    /// Channel normalization.
    pub normalization: Normalization,
    /// Pad on the right with the last column instead of zeros.
    pub pad_edge: bool,
}

/// Packs `images` into `(shape, data, widths)`; `widths` are the content widths
/// before padding.
pub fn batch(images: &[RgbImage], spec: &BatchSpec) -> (Vec<i64>, Vec<f32>, Vec<u32>) {
    let height = spec.height.max(1);
    let channels = if spec.grayscale { 1 } else { 3 };
    let mut widths: Vec<u32> = images
        .iter()
        .map(|image| {
            (f64::from(image.width()) * f64::from(height) / f64::from(image.height().max(1)))
                .ceil()
                .clamp(1.0, 4096.0) as u32
        })
        .collect();
    let width = spec
        .fixed_width
        .unwrap_or_else(|| widths.iter().copied().max().unwrap_or(1).div_ceil(8) * 8);
    let (h, wd) = (height as usize, width as usize);
    let item = channels * h * wd;
    let mut data = vec![0.0; images.len() * item];
    for (i, image) in images.iter().enumerate() {
        widths[i] = widths[i].min(width);
        let w = widths[i] as usize;
        let filter = if spec.grayscale {
            FilterType::CatmullRom
        } else {
            FilterType::Triangle
        };
        let resized = imageops::resize(image, w as u32, height, filter);
        for c in 0..channels {
            for y in 0..h {
                let row = i * item + c * h * wd + y * wd;
                for x in 0..w {
                    let p = resized.get_pixel(x as u32, y as u32);
                    data[row + x] = if spec.grayscale {
                        let luma = 0.299 * f32::from(p[0])
                            + 0.587 * f32::from(p[1])
                            + 0.114 * f32::from(p[2]);
                        normalize(luma / 255.0, 0, Normalization::Half)
                    } else {
                        normalize(f32::from(p[c]) / 255.0, c, spec.normalization)
                    };
                }
                if spec.pad_edge && w < wd && w > 0 {
                    let last = data[row + w - 1];
                    data[row + w..row + wd].fill(last);
                }
            }
        }
    }
    (
        vec![
            images.len() as i64,
            channels as i64,
            i64::from(height),
            i64::from(width),
        ],
        data,
        widths,
    )
}

/// Shrink so the longest side is at most `max_side`. A smaller image is cloned.
///
/// The filter is triangle, matching the native prep. A zero `max_side` or an
/// empty image is returned unchanged.
pub fn cap(image: &RgbImage, max_side: u32) -> RgbImage {
    let longest = image.width().max(image.height());
    if longest == 0 || max_side == 0 {
        return image.clone();
    }
    let ratio = (f64::from(max_side) / f64::from(longest)).min(1.0);
    if ratio >= 1.0 {
        return image.clone();
    }
    imageops::resize(
        image,
        (f64::from(image.width()) * ratio).round().max(1.0) as u32,
        (f64::from(image.height()) * ratio).round().max(1.0) as u32,
        FilterType::Triangle,
    )
}

/// Resize so width and height are multiples of 32, rounding up.
pub fn detection(image: &RgbImage) -> RgbImage {
    let width = image.width().div_ceil(32) * 32;
    let height = image.height().div_ceil(32) * 32;
    if width == image.width() && height == image.height() {
        return image.clone();
    }
    imageops::resize(image, width, height, FilterType::Triangle)
}

/// Pack an RGB image as CHW `f32`.
///
/// `for_detection` selects ImageNet normalization
/// (`mean = [0.485, 0.456, 0.406]`, `std = [0.229, 0.224, 0.225]`).
/// Otherwise each channel is `(x - 0.5) / 0.5`.
pub fn tensor(image: &RgbImage, for_detection: bool) -> Vec<f32> {
    let plane = image.width() as usize * image.height() as usize;
    let mut data = vec![0.0; plane * 3];
    let mean = [0.485, 0.456, 0.406];
    let std = [0.229, 0.224, 0.225];
    for (index, pixel) in image.pixels().enumerate() {
        for channel in 0..3 {
            let unit = f32::from(pixel[channel]) / 255.0;
            data[channel * plane + index] = if for_detection {
                (unit - mean[channel]) / std[channel]
            } else {
                (unit - 0.5) / 0.5
            };
        }
    }
    data
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, RgbImage};

    #[test]
    fn crop_rectifies_and_batch_pads_to_eight() {
        let mut image = RgbImage::from_pixel(40, 20, Rgb([255, 255, 255]));
        for x in 10..30 {
            image.put_pixel(x, 10, Rgb([0, 0, 0]));
        }
        let quad = [[5.0, 5.0], [35.0, 5.0], [35.0, 15.0], [5.0, 15.0]];
        let (crop, q) = crop(&image, quad).expect("crop");
        assert_eq!(crop.dimensions(), (30, 10));
        assert_eq!(q, quad);
        let tall = [[5.0, 0.0], [10.0, 0.0], [10.0, 19.0], [5.0, 19.0]];
        let (rotated, _) = super::crop(&image, tall).expect("tall");
        assert!(rotated.width() > rotated.height());

        let spec = BatchSpec {
            height: 4,
            fixed_width: None,
            grayscale: false,
            normalization: Normalization::Half,
            pad_edge: false,
        };
        let (shape, data, widths) = batch(&[crop.clone(), crop], &spec);
        assert_eq!(shape, vec![2, 3, 4, 16]);
        assert_eq!(widths, vec![12, 12]);
        assert_eq!(data.len(), 2 * 3 * 4 * 16);
        assert_eq!(data[15], 0.0, "zero padding past the content");
    }

    #[test]
    fn cap_shrinks_the_longest_side() {
        let image = RgbImage::from_pixel(40, 20, Rgb([1, 2, 3]));
        let capped = cap(&image, 10);
        assert_eq!(capped.dimensions(), (10, 5));
        assert_eq!(cap(&image, 80).dimensions(), (40, 20));
    }

    #[test]
    fn detection_pads_up_to_a_multiple_of_32() {
        let image = RgbImage::from_pixel(10, 33, Rgb([0, 0, 0]));
        assert_eq!(detection(&image).dimensions(), (32, 64));
        let aligned = RgbImage::from_pixel(32, 32, Rgb([0, 0, 0]));
        assert_eq!(detection(&aligned).dimensions(), (32, 32));
    }

    #[test]
    fn tensor_is_chw_with_the_two_normalizations() {
        let image = RgbImage::from_pixel(1, 1, Rgb([255, 255, 255]));
        let recognizer = tensor(&image, false);
        assert_eq!(recognizer, vec![1.0, 1.0, 1.0]);
        let detector = tensor(&image, true);
        let expected = (1.0 - 0.485) / 0.229;
        assert!((detector[0] - expected).abs() < 1e-5);
        assert_eq!(detector.len(), 3);
    }
}
