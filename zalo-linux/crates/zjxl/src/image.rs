//! Decode, resize, and encode pixels. This is the native `zjxl` image path
//! without the Node-API types.

use std::io::{BufReader, Cursor};

use fast_image_resize as fir;
use jxl_encoder::{LosslessConfig, PixelLayout};
use jxl_oxide::JxlImage;

use crate::error::JxlError;

/// Interleaved 8-bit pixels. `channels` is 1, 2, 3, or 4.
#[derive(Clone, Debug)]
pub struct Pixels {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Samples per pixel.
    pub channels: u32,
    /// Row-major samples.
    pub data: Vec<u8>,
}

/// Size and EXIF-style orientation read from a JPEG XL header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JxlInfo {
    /// Oriented width.
    pub width: u32,
    /// Oriented height.
    pub height: u32,
    /// Orientation tag from the image header. `1` is upright.
    pub orientation: u32,
}

/// Decode the first frame. Channel counts other than 1..=4 are rejected.
pub fn decode(bytes: &[u8]) -> Result<Pixels, JxlError> {
    let image = JxlImage::builder()
        .read(BufReader::new(Cursor::new(bytes)))
        .map_err(|error| JxlError::new(format!("cannot decode JXL: {error}")))?;
    let frame = image
        .render_frame(0)
        .map_err(|error| JxlError::new(format!("cannot render JXL: {error}")))?;
    let mut stream = frame.stream();
    let (width, height, channels) = (stream.width(), stream.height(), stream.channels());
    if !(1..=4).contains(&channels) {
        return Err(JxlError::new("unsupported JXL channel count"));
    }
    let samples = (width as usize)
        .checked_mul(height as usize)
        .and_then(|count| count.checked_mul(channels as usize))
        .ok_or_else(|| JxlError::new("image dimensions overflow"))?;
    let mut data = vec![0; samples];
    let mut filled = 0;
    while filled < data.len() {
        let written = stream.write_to_buffer(&mut data[filled..]);
        if written == 0 {
            return Err(JxlError::new("incomplete pixel buffer rendered from JXL"));
        }
        filled += written;
    }
    Ok(Pixels {
        width,
        height,
        channels,
        data,
    })
}

/// Read width, height, and orientation without rendering pixels.
pub fn dimensions(bytes: &[u8]) -> Result<JxlInfo, JxlError> {
    let image = JxlImage::builder()
        .read(BufReader::new(Cursor::new(bytes)))
        .map_err(|error| JxlError::new(format!("cannot read JXL metadata: {error}")))?;
    Ok(JxlInfo {
        width: image.width(),
        height: image.height(),
        orientation: image.image_header().metadata.orientation,
    })
}

/// Resize. A non-positive axis means "derive it". Both non-positive keeps the source size.
pub fn resize(
    mut pixels: Pixels,
    wanted_width: i32,
    wanted_height: i32,
) -> Result<Pixels, JxlError> {
    let (width, height) =
        output_dimensions(pixels.width, pixels.height, wanted_width, wanted_height);
    if (width, height) == (pixels.width, pixels.height) {
        return Ok(pixels);
    }
    let pixel_type = match pixels.channels {
        1 => fir::PixelType::U8,
        2 => fir::PixelType::U8x2,
        3 => fir::PixelType::U8x3,
        4 => fir::PixelType::U8x4,
        _ => return Err(JxlError::new("unsupported pixel layout")),
    };
    let source =
        fir::images::Image::from_vec_u8(pixels.width, pixels.height, pixels.data, pixel_type)
            .map_err(|error| JxlError::new(format!("invalid source pixels: {error}")))?;
    let mut destination = fir::images::Image::new(width, height, pixel_type);
    fir::Resizer::new()
        .resize(&source, &mut destination, None)
        .map_err(|error| JxlError::new(format!("resize failed: {error}")))?;
    pixels.width = width;
    pixels.height = height;
    pixels.data = destination.into_vec();
    Ok(pixels)
}

/// Encode JPEG. `quality` is clamped to 0..=1 and then scaled to 0..=100.
///
/// One channel, or the first of two, is repeated into RGB. Extra channels are dropped.
pub fn encode_jpeg(pixels: &Pixels, quality: f64) -> Result<Vec<u8>, JxlError> {
    let mut output = Vec::new();
    let quality_byte = (quality.clamp(0.0, 1.0) * 100.0).round() as u8;
    let encoder = jpeg_encoder::Encoder::new(&mut output, quality_byte);
    encoder
        .encode(
            &rgb_pixels(pixels),
            pixels.width as u16,
            pixels.height as u16,
            jpeg_encoder::ColorType::Rgb,
        )
        .map_err(|error| JxlError::new(format!("JPEG encode failed: {error}")))?;
    Ok(output)
}

/// Lossless JPEG XL. The buffer must be exactly `width * height * 3` or `* 4` bytes.
pub fn encode_jxl(buffer: &[u8], width: u32, height: u32) -> Result<Vec<u8>, JxlError> {
    let samples = (width as usize)
        .checked_mul(height as usize)
        .ok_or_else(|| JxlError::new("image dimensions overflow"))?;
    let layout = match buffer.len() {
        len if len == samples * 3 => PixelLayout::Rgb8,
        len if len == samples * 4 => PixelLayout::Rgba8,
        _ => {
            return Err(JxlError::new(
                "bitmap buffer must be exactly width*height*3 or width*height*4 bytes",
            ))
        }
    };
    LosslessConfig::new()
        .encode(buffer, width, height, layout)
        .map_err(|error| JxlError::new(format!("JXL encode failed: {error}")))
}

pub(crate) fn output_dimensions(
    source_width: u32,
    source_height: u32,
    wanted_width: i32,
    wanted_height: i32,
) -> (u32, u32) {
    if wanted_width <= 0 && wanted_height <= 0 {
        return (source_width, source_height);
    }
    let ratio = source_width as f64 / source_height as f64;
    match (wanted_width > 0, wanted_height > 0) {
        (true, true) => (wanted_width as u32, wanted_height as u32),
        (true, false) => (
            wanted_width as u32,
            ((wanted_width as f64 / ratio).round() as u32).max(1),
        ),
        (false, true) => (
            ((wanted_height as f64 * ratio).round() as u32).max(1),
            wanted_height as u32,
        ),
        (false, false) => unreachable!(),
    }
}

fn rgb_pixels(pixels: &Pixels) -> Vec<u8> {
    pixels
        .data
        .chunks_exact(pixels.channels as usize)
        .flat_map(|pixel| match pixels.channels {
            1 | 2 => [pixel[0], pixel[0], pixel[0]],
            _ => [pixel[0], pixel[1], pixel[2]],
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_axis_keeps_the_aspect_ratio() {
        assert_eq!(output_dimensions(4, 2, -1, -1), (4, 2));
        assert_eq!(output_dimensions(4, 2, 2, -1), (2, 1));
        assert_eq!(output_dimensions(4, 2, -1, 2), (4, 2));
        assert_eq!(output_dimensions(4, 2, 3, 5), (3, 5));
    }

    #[test]
    fn gray_and_two_channel_pixels_repeat_the_first_sample() {
        let gray = Pixels {
            width: 1,
            height: 1,
            channels: 1,
            data: vec![40],
        };
        let jpeg = encode_jpeg(&gray, 0.95).unwrap();
        assert_eq!(&jpeg[..2], &[0xff, 0xd8]);
        let two = Pixels {
            width: 1,
            height: 1,
            channels: 2,
            data: vec![40, 200],
        };
        let jpeg = encode_jpeg(&two, 0.95).unwrap();
        assert_eq!(&jpeg[..2], &[0xff, 0xd8]);
    }
}
