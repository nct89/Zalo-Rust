//! `models.json`: which open ONNX files make up the OCR pipeline.
//!
//! ```json
//! {
//!   "detector": "det.onnx",            DB detector, [1,3,H,W] -> [1,1,H,W]
//!   "recognizer": "rec.onnx",          CTC recognizer, [N,C,H,W] -> [N,T,dictionary+1]
//!   "classifier": "cls.onnx",          line orientation, [1,3,h,w] -> [1,2]
//!   "dictionary": "dict.txt",          one symbol per line, class i+1 (0 = blank)
//!   "recognizerHeight": 48,
//!   "recognizerChannels": 3,           3 = RGB, 1 = luma
//!   "recognizerPadding": "zero",       or "edge"
//!   "classifierNormalization": "half", or "imagenet"
//!   "wordSeparator": null,             dictionary symbol that means a space
//!   "allowedCharacters": null          optional file: the only symbols to output
//! }
//! ```
//!
//! Paths are relative to the manifest and may not leave its directory.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;

use crate::prep::Normalization;
use crate::OcrError;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Manifest {
    detector: String,
    recognizer: String,
    classifier: String,
    dictionary: String,
    #[serde(default = "default_height")]
    recognizer_height: u32,
    #[serde(default = "default_channels")]
    recognizer_channels: u32,
    #[serde(default = "default_padding")]
    recognizer_padding: String,
    #[serde(default = "default_normalization")]
    classifier_normalization: String,
    #[serde(default)]
    word_separator: Option<String>,
    #[serde(default)]
    allowed_characters: Option<String>,
}

fn default_height() -> u32 {
    48
}
fn default_channels() -> u32 {
    3
}
fn default_padding() -> String {
    "zero".into()
}
fn default_normalization() -> String {
    "half".into()
}

/// Model files and settings read from a manifest.
#[derive(Clone, Debug)]
pub struct ModelSet {
    /// Detector ONNX path.
    pub detector: PathBuf,
    /// Recognizer ONNX path.
    pub recognizer: PathBuf,
    /// Orientation classifier ONNX path.
    pub classifier: PathBuf,
    /// Dictionary symbols; class `i + 1` reads `dictionary[i]`.
    pub dictionary: Vec<String>,
    /// Recognizer input height.
    pub recognizer_height: u32,
    /// Recognizer takes one luma channel.
    pub grayscale: bool,
    /// Pad recognizer batches with the last column.
    pub pad_edge: bool,
    /// Classifier normalization.
    pub classifier_normalization: Normalization,
    /// Symbol that stands for a space.
    pub word_separator: Option<String>,
    /// Per CTC class (0 = blank) whether it may be output.
    pub allowed: Option<Vec<bool>>,
}

fn bad(text: impl Into<String>) -> OcrError {
    OcrError::Model(text.into())
}

fn inside(dir: &Path, file: &str) -> Result<PathBuf, OcrError> {
    let rel = Path::new(file);
    if !rel.is_relative() || !rel.components().all(|c| matches!(c, Component::Normal(_))) {
        return Err(bad(format!(
            "model path must stay beside models.json: {file}"
        )));
    }
    let path = dir.join(rel);
    if !path.is_file() {
        return Err(bad(format!("missing model file {file}")));
    }
    Ok(path)
}

fn read_text(path: &Path, limit: u64) -> Result<String, OcrError> {
    let size = std::fs::metadata(path)
        .map_err(|e| bad(e.to_string()))?
        .len();
    if size > limit {
        return Err(bad(format!(
            "{} is larger than {limit} bytes",
            path.display()
        )));
    }
    std::fs::read_to_string(path).map_err(|e| bad(format!("{}: {e}", path.display())))
}

impl ModelSet {
    /// Reads `manifest` and checks that every file exists.
    ///
    /// # Errors
    ///
    /// Returns [`OcrError::Model`] for a bad manifest, a missing file, or an
    /// invalid dictionary.
    pub fn load(manifest: &Path) -> Result<Self, OcrError> {
        let dir = manifest.parent().unwrap_or(Path::new("."));
        let text = read_text(manifest, 64 << 10)?;
        let m: Manifest =
            serde_json::from_str(&text).map_err(|e| bad(format!("models.json: {e}")))?;
        if !(8..=128).contains(&m.recognizer_height) {
            return Err(bad("recognizerHeight must be 8..=128"));
        }
        if m.recognizer_channels != 1 && m.recognizer_channels != 3 {
            return Err(bad("recognizerChannels must be 1 or 3"));
        }
        let pad_edge = match m.recognizer_padding.as_str() {
            "zero" => false,
            "edge" => true,
            _ => return Err(bad("recognizerPadding must be \"zero\" or \"edge\"")),
        };
        let classifier_normalization = match m.classifier_normalization.as_str() {
            "half" => Normalization::Half,
            "imagenet" => Normalization::ImageNet,
            _ => {
                return Err(bad(
                    "classifierNormalization must be \"half\" or \"imagenet\"",
                ))
            }
        };
        let text = read_text(&inside(dir, &m.dictionary)?, 4 << 20)?;
        let dictionary = parse_dictionary(&text)?;
        let allowed = match &m.allowed_characters {
            None => None,
            Some(file) => {
                let text = read_text(&inside(dir, file)?, 1 << 20)?;
                Some(allowed_mask(&dictionary, &text)?)
            }
        };
        Ok(Self {
            detector: inside(dir, &m.detector)?,
            recognizer: inside(dir, &m.recognizer)?,
            classifier: inside(dir, &m.classifier)?,
            dictionary,
            recognizer_height: m.recognizer_height,
            grayscale: m.recognizer_channels == 1,
            pad_edge,
            classifier_normalization,
            word_separator: m.word_separator,
            allowed,
        })
    }
}

/// One symbol per line. A line may be a single space; only line breaks are removed.
fn parse_dictionary(text: &str) -> Result<Vec<String>, OcrError> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut lines: Vec<String> = text
        .split('\n')
        .map(|s| s.trim_end_matches('\r').to_owned())
        .collect();
    if lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    if lines.is_empty() || lines.len() >= 100_000 || lines.iter().any(String::is_empty) {
        return Err(bad("invalid dictionary"));
    }
    Ok(lines)
}

fn allowed_mask(dictionary: &[String], text: &str) -> Result<Vec<bool>, OcrError> {
    let set: HashSet<char> = text.chars().filter(|&c| c != '\n' && c != '\r').collect();
    let mut mask = vec![true];
    mask.extend(
        dictionary
            .iter()
            .map(|s| s.chars().all(|c| set.contains(&c))),
    );
    if !mask.iter().skip(1).any(|&a| a) {
        return Err(bad("no dictionary symbol is allowed"));
    }
    Ok(mask)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_resolves_files_and_dictionary() {
        let dir = std::env::temp_dir().join(format!("zocr-manifest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        for name in ["d.onnx", "r.onnx", "c.onnx"] {
            std::fs::write(dir.join(name), b"x").expect("model");
        }
        std::fs::write(dir.join("dict.txt"), "a\n \nb\n").expect("dict");
        std::fs::write(dir.join("allowed.txt"), "ab").expect("allowed");
        std::fs::write(
            dir.join("models.json"),
            r#"{"detector":"d.onnx","recognizer":"r.onnx","classifier":"c.onnx",
                "dictionary":"dict.txt","classifierNormalization":"imagenet",
                "allowedCharacters":"allowed.txt"}"#,
        )
        .expect("manifest");
        let set = ModelSet::load(&dir.join("models.json")).expect("load");
        assert_eq!(set.dictionary, vec!["a", " ", "b"]);
        assert_eq!(set.recognizer_height, 48);
        assert_eq!(set.classifier_normalization, Normalization::ImageNet);
        assert_eq!(set.allowed, Some(vec![true, true, false, true]));

        std::fs::write(
            dir.join("models.json"),
            r#"{"detector":"../d.onnx","recognizer":"r.onnx","classifier":"c.onnx","dictionary":"dict.txt"}"#,
        )
        .expect("manifest");
        assert!(matches!(
            ModelSet::load(&dir.join("models.json")),
            Err(OcrError::Model(_))
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
