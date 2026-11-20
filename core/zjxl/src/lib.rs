//! JPEG XL helpers from `nativelibs/zjxl`.
//!
//! Node-API callbacks are not included. Decode uses `jxl-oxide`, resize uses
//! `fast_image_resize`, JPEG uses `jpeg-encoder`, and lossless encode uses
//! `jxl-encoder`.

#![forbid(unsafe_code)]

mod error;
mod image;
mod service;

pub use error::JxlError;
pub use image::{decode, dimensions, encode_jpeg, encode_jxl, resize, JxlInfo, Pixels};
pub use service::{
    bitmap_to_jxl, decompress_multi, get_info, resize_jxl, resize_jxl_limit, to_jpeg,
    to_jpeg_from_local_path, BitmapToJxl, DecompressMulti, DecompressOutput, DecompressTask,
    JxlToJpeg, LocalPathOutput, LocalPathRequest, ResizeJxl, ResizeJxlLimit,
};
