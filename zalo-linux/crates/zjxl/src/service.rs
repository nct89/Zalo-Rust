//! The calls the native addon exposed, without Node callbacks.
//!
//! `resize_jxl_limit` ignores `limit`, matching the native wrapper: it resizes
//! to the requested width and height and encodes lossless JPEG XL again.

use std::fs;

use crate::error::JxlError;
use crate::image::{decode, dimensions, encode_jpeg, encode_jxl, resize, JxlInfo};

/// Options for [`to_jpeg`]. A missing size means keep that axis.
#[derive(Clone, Debug)]
pub struct JxlToJpeg {
    /// JPEG XL bytes.
    pub buffer: Vec<u8>,
    /// Quality in 0..=1. Missing means `0.95`.
    pub quality: Option<f64>,
    /// Output width. Missing or non-positive keeps or derives the axis.
    pub output_width: Option<i32>,
    /// Output height. Missing or non-positive keeps or derives the axis.
    pub output_height: Option<i32>,
}

/// Raw RGB or RGBA bitmap to encode as lossless JPEG XL.
#[derive(Clone, Debug)]
pub struct BitmapToJxl {
    /// `width * height * 3` or `width * height * 4` bytes.
    pub buffer: Vec<u8>,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

/// Resize a JPEG XL image to an exact size and encode it again.
#[derive(Clone, Debug)]
pub struct ResizeJxl {
    /// JPEG XL bytes.
    pub buffer: Vec<u8>,
    /// Destination width.
    pub width: u32,
    /// Destination height.
    pub height: u32,
}

/// Same as [`ResizeJxl`]. `limit` is accepted and ignored.
#[derive(Clone, Debug)]
pub struct ResizeJxlLimit {
    /// JPEG XL bytes.
    pub buffer: Vec<u8>,
    /// Destination width.
    pub width: u32,
    /// Destination height.
    pub height: u32,
    /// Present for compatibility. The native wrapper does not apply it.
    pub limit: Option<u32>,
}

/// Read a JPEG XL file and write JPEG bytes.
#[derive(Clone, Debug)]
pub struct LocalPathRequest {
    /// Source path.
    pub local_path: String,
    /// Quality in 0..=1. Missing means `0.95`.
    pub quality: Option<f64>,
    /// Maximum width. Missing means keep that axis.
    pub max_width: Option<i32>,
    /// Maximum height. Missing means keep that axis.
    pub max_height: Option<i32>,
    /// When set, the full JPEG is also written here.
    pub output_path: Option<String>,
    /// When true, also return a second JPEG. Default preview size is 1024×720.
    pub generate_preview: Option<bool>,
    /// Preview width. Missing means 1024 when a preview is requested.
    pub preview_width: Option<i32>,
    /// Preview height. Missing means 720 when a preview is requested.
    pub preview_height: Option<i32>,
}

/// JPEG bytes from [`to_jpeg_from_local_path`].
#[derive(Clone, Debug)]
pub struct LocalPathOutput {
    /// Full-size JPEG.
    pub data: Vec<u8>,
    /// Width of `data`.
    pub width: u32,
    /// Height of `data`.
    pub height: u32,
    /// The path that was written, or empty when nothing was written.
    pub output_path: String,
    /// Preview JPEG when one was requested.
    pub preview: Option<Vec<u8>>,
}

/// One resize target inside [`decompress_multi`].
#[derive(Clone, Debug, Default)]
pub struct DecompressTask {
    /// Used when `width` is missing.
    pub max_width: Option<i32>,
    /// Used when `height` is missing.
    pub max_height: Option<i32>,
    /// Exact width. Wins over `max_width`.
    pub width: Option<i32>,
    /// Exact height. Wins over `max_height`.
    pub height: Option<i32>,
    /// When non-empty, the JPEG is written here.
    pub output_path: Option<String>,
}

/// Several JPEG renders of one JPEG XL buffer or file.
#[derive(Clone, Debug)]
pub struct DecompressMulti {
    /// Source bytes. Wins over `local_path` when both are set.
    pub buffer: Option<Vec<u8>>,
    /// Source path, used when `buffer` is absent.
    pub local_path: Option<String>,
    /// Quality in 0..=1. Missing means `0.95`.
    pub quality: Option<f64>,
    /// Targets. Missing means one task that keeps the original size.
    pub tasks: Option<Vec<DecompressTask>>,
}

/// One JPEG from [`decompress_multi`].
#[derive(Clone, Debug)]
pub struct DecompressOutput {
    /// JPEG bytes.
    pub data: Vec<u8>,
    /// Byte length of `data`.
    pub size: u32,
    /// Written path, or empty.
    pub output_path: String,
    /// Width of the JPEG.
    pub width: u32,
    /// Height of the JPEG.
    pub height: u32,
}

/// Header width, height, and orientation.
pub fn get_info(buffer: &[u8]) -> Result<JxlInfo, JxlError> {
    dimensions(buffer)
}

/// Decode, optionally resize, and encode JPEG. Default quality is `0.95`.
pub fn to_jpeg(options: &JxlToJpeg) -> Result<Vec<u8>, JxlError> {
    let pixels = resize(
        decode(&options.buffer)?,
        options.output_width.unwrap_or(-1),
        options.output_height.unwrap_or(-1),
    )?;
    encode_jpeg(&pixels, options.quality.unwrap_or(0.95))
}

/// Encode an RGB or RGBA bitmap as lossless JPEG XL.
pub fn bitmap_to_jxl(options: &BitmapToJxl) -> Result<Vec<u8>, JxlError> {
    encode_jxl(&options.buffer, options.width, options.height)
}

/// Decode, resize to an exact size, and encode lossless JPEG XL.
pub fn resize_jxl(options: &ResizeJxl) -> Result<Vec<u8>, JxlError> {
    let pixels = resize(
        decode(&options.buffer)?,
        options.width as i32,
        options.height as i32,
    )?;
    encode_jxl(&pixels.data, pixels.width, pixels.height)
}

/// [`resize_jxl`]. The `limit` field is not applied.
pub fn resize_jxl_limit(options: &ResizeJxlLimit) -> Result<Vec<u8>, JxlError> {
    let _ = options.limit;
    resize_jxl(&ResizeJxl {
        buffer: options.buffer.clone(),
        width: options.width,
        height: options.height,
    })
}

/// Decode a file, encode JPEG, and optionally a 1024×720 preview.
pub fn to_jpeg_from_local_path(options: &LocalPathRequest) -> Result<LocalPathOutput, JxlError> {
    let data = fs::read(&options.local_path)
        .map_err(|error| JxlError::new(format!("cannot read localPath: {error}")))?;
    let pixels = resize(
        decode(&data)?,
        options.max_width.unwrap_or(-1),
        options.max_height.unwrap_or(-1),
    )?;
    let quality = options.quality.unwrap_or(0.95);
    let jpeg = encode_jpeg(&pixels, quality)?;
    if let Some(path) = &options.output_path {
        fs::write(path, &jpeg)
            .map_err(|error| JxlError::new(format!("cannot write outputPath: {error}")))?;
    }
    let preview = if options.generate_preview.unwrap_or(false) {
        let preview_pixels = resize(
            pixels.clone(),
            options.preview_width.unwrap_or(1024),
            options.preview_height.unwrap_or(720),
        )?;
        Some(encode_jpeg(&preview_pixels, quality)?)
    } else {
        None
    };
    Ok(LocalPathOutput {
        data: jpeg,
        width: pixels.width,
        height: pixels.height,
        output_path: options.output_path.clone().unwrap_or_default(),
        preview,
    })
}

/// Decode one source into JPEG once per task.
///
/// At least one of `buffer` or `local_path` is required. An empty task list is
/// not the default: a missing list becomes one empty task, which keeps the size.
pub fn decompress_multi(options: &DecompressMulti) -> Result<Vec<DecompressOutput>, JxlError> {
    let source = match (&options.buffer, &options.local_path) {
        (Some(buffer), _) => buffer.clone(),
        (None, Some(path)) => fs::read(path)
            .map_err(|error| JxlError::new(format!("cannot read localPath: {error}")))?,
        (None, None) => return Err(JxlError::new("buffer or localPath is required")),
    };
    let quality = options.quality.unwrap_or(0.95);
    let tasks = options
        .tasks
        .clone()
        .unwrap_or_else(|| vec![DecompressTask::default()]);
    tasks
        .iter()
        .map(|task| decompress_task(&source, task, quality))
        .collect()
}

fn decompress_task(
    source: &[u8],
    task: &DecompressTask,
    quality: f64,
) -> Result<DecompressOutput, JxlError> {
    let pixels = resize(
        decode(source)?,
        task.width.or(task.max_width).unwrap_or(-1),
        task.height.or(task.max_height).unwrap_or(-1),
    )?;
    let jpeg = encode_jpeg(&pixels, quality)?;
    let output_path = task.output_path.clone().unwrap_or_default();
    if !output_path.is_empty() {
        fs::write(&output_path, &jpeg)
            .map_err(|error| JxlError::new(format!("cannot write outputPath: {error}")))?;
    }
    Ok(DecompressOutput {
        size: jpeg.len() as u32,
        data: jpeg,
        output_path,
        width: pixels.width,
        height: pixels.height,
    })
}

#[cfg(test)]
fn sample_rgb_jxl() -> Result<Vec<u8>, JxlError> {
    let pixels = [255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255];
    encode_jxl(&pixels, 2, 2)
}

#[cfg(test)]
fn remove_if_exists(path: &std::path::Path) {
    let _ = fs::remove_file(path);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lossless_roundtrip_keeps_the_bitmap_size() {
        let jxl = sample_rgb_jxl().unwrap();
        let info = get_info(&jxl).unwrap();
        assert_eq!((info.width, info.height), (2, 2));
        let pixels = decode(&jxl).unwrap();
        assert_eq!((pixels.width, pixels.height, pixels.channels), (2, 2, 3));
        assert_eq!(pixels.data.len(), 12);
        let jpeg = to_jpeg(&JxlToJpeg {
            buffer: jxl.clone(),
            quality: None,
            output_width: Some(1),
            output_height: Some(1),
        })
        .unwrap();
        assert_eq!(&jpeg[..2], &[0xff, 0xd8]);
        let resized = resize_jxl_limit(&ResizeJxlLimit {
            buffer: jxl,
            width: 2,
            height: 2,
            limit: Some(1),
        })
        .unwrap();
        let again = decode(&resized).unwrap();
        assert_eq!((again.width, again.height), (2, 2));
    }

    #[test]
    fn local_path_and_multi_write_jpeg() {
        let jxl = sample_rgb_jxl().unwrap();
        let stamp = std::process::id();
        let dir = std::env::temp_dir();
        let src = dir.join(format!("zjxl-{stamp}.jxl"));
        let full = dir.join(format!("zjxl-{stamp}.jpg"));
        let task_out = dir.join(format!("zjxl-{stamp}-task.jpg"));
        fs::write(&src, &jxl).unwrap();
        let local = to_jpeg_from_local_path(&LocalPathRequest {
            local_path: src.to_string_lossy().into_owned(),
            quality: Some(0.95),
            max_width: Some(2),
            max_height: Some(2),
            output_path: Some(full.to_string_lossy().into_owned()),
            generate_preview: Some(true),
            preview_width: Some(2),
            preview_height: Some(1),
        })
        .unwrap();
        assert_eq!(&local.data[..2], &[0xff, 0xd8]);
        assert!(local.preview.unwrap().starts_with(&[0xff, 0xd8]));
        let multi = decompress_multi(&DecompressMulti {
            buffer: None,
            local_path: Some(src.to_string_lossy().into_owned()),
            quality: None,
            tasks: Some(vec![DecompressTask {
                width: Some(2),
                height: Some(2),
                output_path: Some(task_out.to_string_lossy().into_owned()),
                ..DecompressTask::default()
            }]),
        })
        .unwrap();
        assert_eq!(multi.len(), 1);
        assert_eq!(&multi[0].data[..2], &[0xff, 0xd8]);
        assert!(full.is_file());
        remove_if_exists(&src);
        remove_if_exists(&full);
        remove_if_exists(&task_out);
    }

    #[test]
    fn multi_requires_a_source() {
        let error = decompress_multi(&DecompressMulti {
            buffer: None,
            local_path: None,
            quality: None,
            tasks: None,
        })
        .unwrap_err();
        assert_eq!(error.0, "buffer or localPath is required");
    }
}
