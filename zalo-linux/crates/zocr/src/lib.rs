//! Text recognition (OCR) with open PP-OCR models on ONNX Runtime.
//!
//! The pipeline ports `zocr-host` of the Electron build: DB detector, optional
//! 180° line classifier, CTC recognizer. `libonnxruntime.so` is loaded at
//! runtime and the models are listed in `models.json`; neither is linked into
//! the binary.
//!
//! [`find_ocr_dir`] looks for a directory holding both, in this order:
//! `$ZALO_OCR_DIR`, `$XDG_DATA_HOME/zalo-rust/ocr` (or
//! `~/.local/share/zalo-rust/ocr`), `ocr/` beside the executable, and
//! `/usr/share/zalo-rust/ocr`. Inside it, the manifest may be `models.json` or
//! `zocr/models.json` (the Electron app's `native/ocr` layout).

#![forbid(unsafe_code)]

mod ctc;
mod detection;
mod engine;
mod geometry;
mod models;
mod prep;

use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub use ctc::{decode, decode_batch, keep_line, words, CtcError, Decoded, Labels, Token, Word};
pub use detection::{postprocess, Detected};
pub use engine::{Engine, Line, Options};
pub use geometry::{Point, Quad};
use image::RgbImage;
pub use models::ModelSet;
pub use prep::{batch, cap, crop, detection, tensor, BatchSpec, Normalization};

/// Recognition could not run.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum OcrError {
    /// No directory with `libonnxruntime.so` and `models.json` was found.
    #[error("OCR model is not installed (see zocr::find_ocr_dir)")]
    ModelMissing,
    /// `libonnxruntime.so` could not be loaded.
    #[error("ONNX Runtime could not be loaded: {0}")]
    RuntimeMissing(String),
    /// The manifest or a model file is invalid.
    #[error("OCR model: {0}")]
    Model(String),
    /// Inference failed.
    #[error("OCR runtime: {0}")]
    Runtime(String),
}

/// Runtime library and manifest found in one OCR directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OcrInstall {
    /// `libonnxruntime.so`.
    pub runtime: PathBuf,
    /// `models.json`.
    pub manifest: PathBuf,
}

/// The runtime and manifest inside `dir`, if both are there.
pub fn install_in(dir: &Path) -> Option<OcrInstall> {
    let runtime = ["libonnxruntime.so", "libonnxruntime.so.1"]
        .iter()
        .map(|name| dir.join(name))
        .find(|p| p.is_file())?;
    let manifest = [
        dir.join("models.json"),
        dir.join("zocr").join("models.json"),
    ]
    .into_iter()
    .find(|p| p.is_file())?;
    Some(OcrInstall { runtime, manifest })
}

/// Directories searched by [`find_ocr_dir`], in order.
pub fn search_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(dir) = std::env::var_os("ZALO_OCR_DIR").filter(|v| !v.is_empty()) {
        dirs.push(PathBuf::from(dir));
    }
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")));
    if let Some(data) = data {
        dirs.push(data.join("zalo-rust").join("ocr"));
    }
    if let Some(exe_dir) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
    {
        dirs.push(exe_dir.join("ocr"));
    }
    dirs.push(PathBuf::from("/usr/share/zalo-rust/ocr"));
    dirs
}

/// First directory of [`search_dirs`] that holds an install.
pub fn find_ocr_dir() -> Option<OcrInstall> {
    search_dirs().iter().find_map(|dir| install_in(dir))
}

static ENGINE: Mutex<Option<Engine>> = Mutex::new(None);

/// Recognized lines of `image`, joined by newlines. The engine is loaded on
/// first use and kept for the life of the process.
///
/// # Errors
///
/// [`OcrError::ModelMissing`] when nothing is installed, otherwise the error
/// of [`Engine::open`] or [`Engine::recognize`].
pub fn recognize(image: &RgbImage) -> Result<String, OcrError> {
    let lines = recognize_lines(image)?;
    Ok(lines
        .into_iter()
        .map(|line| line.text)
        .collect::<Vec<_>>()
        .join("\n"))
}

/// Like [`recognize`], with scores and boxes.
///
/// # Errors
///
/// See [`recognize`].
pub fn recognize_lines(image: &RgbImage) -> Result<Vec<Line>, OcrError> {
    let mut guard = ENGINE
        .lock()
        .map_err(|_| OcrError::Runtime("engine lock poisoned".into()))?;
    if guard.is_none() {
        let install = find_ocr_dir().ok_or(OcrError::ModelMissing)?;
        *guard = Some(Engine::open(
            &install.runtime,
            &install.manifest,
            Options::default(),
        )?);
    }
    match guard.as_mut() {
        Some(engine) => engine.recognize(image),
        None => Err(OcrError::ModelMissing),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_needs_runtime_and_manifest() {
        let dir = std::env::temp_dir().join(format!("zocr-install-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("zocr")).expect("dir");
        assert_eq!(install_in(&dir), None);
        std::fs::write(dir.join("libonnxruntime.so"), b"").expect("lib");
        assert_eq!(install_in(&dir), None);
        std::fs::write(dir.join("zocr/models.json"), b"{}").expect("manifest");
        let found = install_in(&dir).expect("install");
        assert!(found.manifest.ends_with("zocr/models.json"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Runs the real models on a rendered line. Needs an OCR install
    /// (`ZALO_OCR_DIR` or `~/.local/share/zalo-rust/ocr`) and a test image in
    /// `ZOCR_SAMPLE`. Run with `cargo test -p zocr -- --ignored --nocapture`.
    #[test]
    #[ignore = "needs libonnxruntime.so, models and a sample image"]
    fn recognizes_a_real_image() {
        let path = std::env::var("ZOCR_SAMPLE").expect("ZOCR_SAMPLE");
        let image = image::open(path).expect("open").into_rgb8();
        let lines = recognize_lines(&image).expect("recognize");
        for line in &lines {
            eprintln!("{:.3} {:.3} {}", line.detection, line.score, line.text);
        }
        assert!(!lines.is_empty());
    }
}
