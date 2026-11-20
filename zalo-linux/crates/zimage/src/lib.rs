//! Thumbnail pipeline from `nativelibs/zimage`.
//!
//! The Node-API wrapper is not part of this crate. Bytes are decoded, resized
//! with Lanczos3, and encoded as JPEG (quality 85, alpha flattened onto white)
//! or PNG. Metadata from the input is not copied.

#![forbid(unsafe_code)]

use std::io::Cursor;
use std::path::Path;

use image::imageops::FilterType;
use image::{DynamicImage, ImageEncoder, RgbaImage};

/// Encode choice for a thumbnail.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputFormat {
    /// JPEG, quality 85, opaque RGB.
    Jpeg,
    /// PNG. Alpha is kept when the resized image has it.
    Png,
}

/// Failure while checking, decoding, resizing, or writing a thumbnail.
#[derive(Debug, thiserror::Error)]
pub enum ImageError {
    /// Width or height was zero.
    #[error("width and height must both be greater than zero")]
    Dimensions,
    /// The decoder rejected the input.
    #[error("An error occurred in thumbnailing: {0}")]
    Decode(String),
    /// The encoder rejected the pixels.
    #[error("An error occurred in encoding: {0}")]
    Encode(String),
    /// A filesystem thumbnail could not be read or written.
    #[error("An error occurred in thumbnailing using file system: {0}")]
    File(String),
}

/// `jpeg` and `jpg` select JPEG. Every other value, including none, selects PNG.
pub fn output_format(format: Option<&str>) -> OutputFormat {
    match format.map(str::to_ascii_lowercase).as_deref() {
        Some("jpeg") | Some("jpg") => OutputFormat::Jpeg,
        _ => OutputFormat::Png,
    }
}

/// Reject a zero width or height before any pixel work.
pub fn validate_dimensions(width: u32, height: u32) -> Result<(), ImageError> {
    if width == 0 || height == 0 {
        return Err(ImageError::Dimensions);
    }
    Ok(())
}

/// Blend an RGBA image onto white and return opaque RGB.
///
/// Each channel is `(channel * alpha + 255 * (255 - alpha)) / 255`.
pub fn flatten_alpha_to_white(image: RgbaImage) -> image::RgbImage {
    let (width, height) = image.dimensions();
    let mut output = image::RgbImage::new(width, height);
    for (x, y, pixel) in image.enumerate_pixels() {
        let alpha = u16::from(pixel[3]);
        let blend = |channel: u8| -> u8 {
            ((u16::from(channel) * alpha + 255 * (255 - alpha)) / 255) as u8
        };
        output.put_pixel(
            x,
            y,
            image::Rgb([blend(pixel[0]), blend(pixel[1]), blend(pixel[2])]),
        );
    }
    output
}

/// Decode, resize with Lanczos3, and encode without copying input metadata.
pub fn process_image_bytes(
    input: &[u8],
    width: u32,
    height: u32,
    format: OutputFormat,
) -> Result<Vec<u8>, ImageError> {
    validate_dimensions(width, height)?;
    let decoded =
        image::load_from_memory(input).map_err(|error| ImageError::Decode(error.to_string()))?;
    let resized = decoded.resize_exact(width, height, FilterType::Lanczos3);
    let mut encoded = Vec::new();
    match format {
        OutputFormat::Jpeg => {
            let rgb = flatten_alpha_to_white(resized.to_rgba8());
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut encoded, 85)
                .write_image(rgb.as_raw(), width, height, image::ExtendedColorType::Rgb8)
                .map_err(|error| ImageError::Encode(error.to_string()))?;
        }
        OutputFormat::Png => {
            let png = if resized.color().has_alpha() {
                DynamicImage::ImageRgba8(resized.to_rgba8())
            } else {
                DynamicImage::ImageRgb8(resized.to_rgb8())
            };
            png.write_to(&mut Cursor::new(&mut encoded), image::ImageFormat::Png)
                .map_err(|error| ImageError::Encode(error.to_string()))?;
        }
    }
    Ok(encoded)
}

/// Read `input`, write a thumbnail to `output`.
///
/// `.jpg` and `.jpeg` outputs are JPEG. Any other extension is PNG.
pub fn thumbnail_file(
    input: impl AsRef<Path>,
    output: impl AsRef<Path>,
    width: u32,
    height: u32,
) -> Result<(), ImageError> {
    validate_dimensions(width, height)?;
    let output = output.as_ref();
    let format = output_format(output.extension().and_then(|extension| extension.to_str()));
    let bytes = std::fs::read(input).map_err(|error| ImageError::File(error.to_string()))?;
    let encoded = process_image_bytes(&bytes, width, height, format)?;
    std::fs::write(output, encoded).map_err(|error| ImageError::File(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};

    fn solid_png() -> Vec<u8> {
        let image = RgbaImage::from_pixel(4, 4, Rgba([10, 20, 30, 128]));
        let mut png = Vec::new();
        image
            .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        png
    }

    #[test]
    fn format_names_follow_the_native_helper() {
        assert_eq!(output_format(Some("JPEG")), OutputFormat::Jpeg);
        assert_eq!(output_format(Some("jpg")), OutputFormat::Jpeg);
        assert_eq!(output_format(Some("png")), OutputFormat::Png);
        assert_eq!(output_format(None), OutputFormat::Png);
        assert_eq!(output_format(Some("webp")), OutputFormat::Png);
    }

    #[test]
    fn zero_dimensions_are_rejected() {
        let error = process_image_bytes(&solid_png(), 0, 2, OutputFormat::Png).unwrap_err();
        assert!(matches!(error, ImageError::Dimensions));
    }

    #[test]
    fn alpha_blends_onto_white() {
        let mut image = RgbaImage::new(1, 1);
        image.put_pixel(0, 0, Rgba([100, 0, 0, 128]));
        let rgb = flatten_alpha_to_white(image);
        assert_eq!(rgb.get_pixel(0, 0).0, [177, 127, 127]);
    }

    #[test]
    fn jpeg_thumbnail_starts_with_soi() {
        let jpeg = process_image_bytes(&solid_png(), 2, 2, OutputFormat::Jpeg).unwrap();
        assert_eq!(&jpeg[..2], &[0xff, 0xd8]);
    }

    #[test]
    fn file_worker_picks_jpeg_from_the_output_extension() {
        let dir = std::env::temp_dir();
        let stamp = std::process::id();
        let input = dir.join(format!("zimage-{stamp}.png"));
        let output = dir.join(format!("zimage-{stamp}.jpg"));
        std::fs::write(&input, solid_png()).unwrap();
        thumbnail_file(&input, &output, 2, 2).unwrap();
        let jpeg = std::fs::read(&output).unwrap();
        let _ = std::fs::remove_file(&input);
        let _ = std::fs::remove_file(&output);
        assert_eq!(&jpeg[..2], &[0xff, 0xd8]);
    }
}
