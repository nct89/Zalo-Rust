//! Detector → (orientation) → recognizer → CTC, on ONNX Runtime.
//!
//! `libonnxruntime.so` is loaded at runtime (`ort` `load-dynamic`), so the
//! build does not link or download it.

use std::path::{Path, PathBuf};

use image::imageops::{self, FilterType};
use image::RgbImage;
use ort::environment::Environment;
use ort::session::Session;
use ort::value::Tensor;

use crate::ctc::{decode_batch, keep_line, Labels};
use crate::detection::{postprocess, Detected};
use crate::geometry::Quad;
use crate::models::ModelSet;
use crate::prep::{self, BatchSpec, Normalization};
use crate::OcrError;

/// Tuning, matching the defaults of the `zocr-host` engine.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// Longest side fed to the detector.
    pub max_side: u32,
    /// Run the 180° line classifier.
    pub orientation: bool,
    /// Classifier probability that counts as upside down.
    pub orientation_threshold: f32,
    /// Lines probed by the classifier before applying the majority to all.
    pub orientation_probe: usize,
    /// Drop low-confidence lines.
    pub score_filter: bool,
    /// Detector floor when the recognizer is not sure.
    pub det_min: f32,
    /// Recognizer floor.
    pub rec_min: f32,
    /// Recognizer score that keeps a line whatever the detector says.
    pub rec_keep: f32,
    /// Recognizer batch size for dynamic-batch models.
    pub batch: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            max_side: 1280,
            orientation: false,
            orientation_threshold: 0.9,
            orientation_probe: 5,
            score_filter: true,
            det_min: 0.741,
            rec_min: 0.840,
            rec_keep: 0.925,
            batch: 8,
        }
    }
}

/// One recognized line.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    /// Text.
    pub text: String,
    /// Recognizer confidence.
    pub score: f32,
    /// Detector confidence.
    pub detection: f32,
    /// Corners in source pixels.
    pub quad: Quad,
}

fn rt(err: impl std::fmt::Display) -> OcrError {
    OcrError::Runtime(err.to_string())
}

/// `[N,C,H,W]` input shape with `-1` for dynamic dimensions.
fn input_shape(session: &Session, index: usize) -> Vec<i64> {
    session
        .inputs()
        .get(index)
        .and_then(|outlet| outlet.dtype().tensor_shape())
        .map(|shape| shape.to_vec())
        .unwrap_or_default()
}

fn dim(shape: &[i64], index: usize) -> Option<u32> {
    shape
        .get(index)
        .copied()
        .filter(|d| *d > 0)
        .and_then(|d| u32::try_from(d).ok())
}

/// The `seg` input of segment-aware recognizers: 0 for content columns
/// (in steps of 8 pixels), 1 for right-hand padding.
fn segments(shape: &[i64], widths: &[u32]) -> Option<(Vec<i64>, Vec<i32>)> {
    if shape.len() != 4 || shape[3] % 8 != 0 || widths.len() as i64 != shape[0] {
        return None;
    }
    let steps = usize::try_from(shape[3] / 8).ok()?;
    let mut data = vec![1; widths.len() * steps];
    for (i, &width) in widths.iter().enumerate() {
        let valid = (width.div_ceil(8) as usize).min(steps);
        data[i * steps..i * steps + valid].fill(0);
    }
    Some((vec![shape[0], steps as i64], data))
}

fn run_one(
    session: &mut Session,
    shape: Vec<i64>,
    data: Vec<f32>,
) -> Result<(Vec<i64>, Vec<f32>), OcrError> {
    let tensor = Tensor::from_array((shape, data)).map_err(rt)?;
    let outputs = session.run(ort::inputs![tensor]).map_err(rt)?;
    let (shape, values) = outputs[0].try_extract_tensor::<f32>().map_err(rt)?;
    Ok((shape.to_vec(), values.to_vec()))
}

/// Loaded models.
pub struct Engine {
    _environment: Environment,
    detector: Session,
    recognizer: Session,
    classifier: Session,
    models: ModelSet,
    options: Options,
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("dictionary", &self.models.dictionary.len())
            .field("options", &self.options)
            .finish_non_exhaustive()
    }
}

impl Engine {
    /// Loads `runtime` (`libonnxruntime.so`) and the models of `manifest`.
    ///
    /// The runtime library can only be loaded once per process.
    ///
    /// # Errors
    ///
    /// [`OcrError::RuntimeMissing`] when the library cannot be loaded,
    /// [`OcrError::Model`] for a bad manifest or model, and
    /// [`OcrError::Runtime`] when ONNX Runtime rejects a model.
    pub fn open(runtime: &Path, manifest: &Path, options: Options) -> Result<Self, OcrError> {
        let models = ModelSet::load(manifest)?;
        let environment = ort::init_from(runtime)
            .map_err(|e| OcrError::RuntimeMissing(format!("{}: {e}", runtime.display())))?
            .with_name("zocr")
            .build()
            .map_err(rt)?;
        let threads = std::thread::available_parallelism()
            .map(|n| n.get().min(4))
            .unwrap_or(2);
        let session = |path: &PathBuf| -> Result<Session, OcrError> {
            let bytes = std::fs::read(path).map_err(|e| OcrError::Model(e.to_string()))?;
            Session::builder(&environment)
                .map_err(rt)?
                .with_intra_threads(threads)
                .map_err(rt)?
                .commit_from_memory(&bytes)
                .map_err(|e| OcrError::Model(format!("{}: {e}", path.display())))
        };
        let detector = session(&models.detector)?;
        let recognizer = session(&models.recognizer)?;
        let classifier = session(&models.classifier)?;
        let rec_inputs = recognizer.inputs().len();
        if detector.inputs().len() != 1
            || classifier.inputs().len() != 1
            || !(rec_inputs == 1 || (rec_inputs == 2 && recognizer.inputs()[1].name() == "seg"))
        {
            return Err(OcrError::Model("unsupported model inputs".into()));
        }
        Ok(Self {
            _environment: environment,
            detector,
            recognizer,
            classifier,
            models,
            options,
        })
    }

    /// Recognizes the lines of `source`, top to bottom.
    ///
    /// # Errors
    ///
    /// [`OcrError::Runtime`] when inference fails or a model output has an
    /// unexpected shape.
    pub fn recognize(&mut self, source: &RgbImage) -> Result<Vec<Line>, OcrError> {
        if source.width() == 0 || source.height() == 0 {
            return Ok(Vec::new());
        }
        let detected = self.detect(source)?;
        let mut crops = Vec::with_capacity(detected.len());
        let mut kept = Vec::with_capacity(detected.len());
        for det in detected {
            if let Some((crop, _)) = prep::crop(source, det.quad) {
                crops.push(crop);
                kept.push(det);
            }
        }
        if crops.is_empty() {
            return Ok(Vec::new());
        }
        if self.options.orientation {
            self.orient(&mut crops)?;
        }
        let decoded = self.read(&crops)?;
        let o = self.options;
        Ok(kept
            .into_iter()
            .zip(decoded)
            .filter_map(|(det, (text, score))| {
                let keep = !text.trim().is_empty()
                    && keep_line(
                        o.score_filter,
                        det.score,
                        score,
                        o.det_min,
                        o.rec_min,
                        o.rec_keep,
                    );
                keep.then_some(Line {
                    text,
                    score,
                    detection: det.score,
                    quad: det.quad,
                })
            })
            .collect())
    }

    fn detect(&mut self, source: &RgbImage) -> Result<Vec<Detected>, OcrError> {
        let capped = prep::cap(source, self.options.max_side);
        let expected = input_shape(&self.detector, 0);
        let aligned = prep::detection(&capped);
        let width = dim(&expected, 3).unwrap_or(aligned.width());
        let height = dim(&expected, 2).unwrap_or(aligned.height());
        let resized = if aligned.dimensions() == (width, height) {
            aligned
        } else {
            imageops::resize(&capped, width, height, FilterType::Triangle)
        };
        let input = prep::tensor(&resized, true);
        let shape = vec![1, 3, i64::from(height), i64::from(width)];
        let (out_shape, map) = run_one(&mut self.detector, shape, input)?;
        let [1, 1, h, w] = out_shape[..] else {
            return Err(rt("detector expected [1,1,H,W] output"));
        };
        postprocess(
            &map,
            usize::try_from(w).map_err(rt)?,
            usize::try_from(h).map_err(rt)?,
            source.width(),
            source.height(),
        )
        .ok_or_else(|| rt("detector map size"))
    }

    /// Flips every crop by 180° when most probed lines read upside down.
    fn orient(&mut self, crops: &mut [RgbImage]) -> Result<(), OcrError> {
        let expected = input_shape(&self.classifier, 0);
        let spec = BatchSpec {
            height: dim(&expected, 2).unwrap_or(80),
            fixed_width: Some(dim(&expected, 3).unwrap_or(160)),
            grayscale: false,
            normalization: self.models.classifier_normalization,
            pad_edge: false,
        };
        let probe = crops.len().min(self.options.orientation_probe);
        let mut flipped = 0;
        for crop in crops.iter().take(probe) {
            let (shape, data, _) = prep::batch(std::slice::from_ref(crop), &spec);
            let (out_shape, row) = run_one(&mut self.classifier, shape, data)?;
            if out_shape != [1, 2] || row.len() != 2 {
                return Err(rt("classifier expected [1,2] output"));
            }
            let sum = row[0] + row[1];
            let p = if row.iter().all(|v| (0.0..=1.0).contains(v)) && (sum - 1.0).abs() < 0.01 {
                row[1]
            } else {
                1.0 / (1.0 + (row[0] - row[1]).exp())
            };
            if p >= self.options.orientation_threshold {
                flipped += 1;
            }
        }
        if probe > 0 && flipped * 2 > probe {
            for crop in crops.iter_mut() {
                *crop = imageops::rotate180(crop);
            }
        }
        Ok(())
    }

    fn read(&mut self, crops: &[RgbImage]) -> Result<Vec<(String, f32)>, OcrError> {
        let expected = input_shape(&self.recognizer, 0);
        let fixed_batch = dim(&expected, 0).map(|n| n as usize);
        let batch_size = fixed_batch.unwrap_or(self.options.batch).clamp(1, 16);
        let spec = BatchSpec {
            height: self.models.recognizer_height,
            fixed_width: dim(&expected, 3),
            grayscale: self.models.grayscale,
            normalization: Normalization::Half,
            pad_edge: self.models.pad_edge,
        };
        // Similar widths in one batch waste less padding.
        let mut order: Vec<usize> = (0..crops.len()).collect();
        order.sort_by_key(|&i| {
            u64::from(crops[i].width()) * 48 / u64::from(crops[i].height().max(1))
        });
        let mut out = vec![(String::new(), 0.0); crops.len()];
        let labels = Labels {
            dictionary: &self.models.dictionary,
            separator: self.models.word_separator.as_deref(),
            allowed: self.models.allowed.as_deref(),
        };
        let with_seg = self.recognizer.inputs().len() == 2;
        for indices in order.chunks(batch_size) {
            let mut images: Vec<RgbImage> = indices.iter().map(|&i| crops[i].clone()).collect();
            if let Some(n) = fixed_batch {
                while images.len() < n {
                    let last = images[images.len() - 1].clone();
                    images.push(last);
                }
            }
            let (shape, data, widths) = prep::batch(&images, &spec);
            let padded = u32::try_from(shape[3]).map_err(rt)?;
            let (out_shape, values) = if with_seg {
                let (seg_shape, seg) = segments(&shape, &widths).ok_or_else(|| rt("seg input"))?;
                let image = Tensor::from_array((shape, data)).map_err(rt)?;
                let seg = Tensor::from_array((seg_shape, seg)).map_err(rt)?;
                let outputs = self.recognizer.run(ort::inputs![image, seg]).map_err(rt)?;
                let (s, v) = outputs[0].try_extract_tensor::<f32>().map_err(rt)?;
                (s.to_vec(), v.to_vec())
            } else {
                run_one(&mut self.recognizer, shape, data)?
            };
            let items = decode_batch(&values, &out_shape, images.len(), &widths, padded, &labels)
                .map_err(rt)?;
            for (&index, item) in indices.iter().zip(items) {
                out[index] = (item.text, item.score);
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_mark_padding_per_item() {
        let (shape, data) = segments(&[2, 3, 48, 24], &[24, 9]).expect("seg");
        assert_eq!(shape, vec![2, 3]);
        assert_eq!(data, vec![0, 0, 0, 0, 0, 1]);
        assert!(segments(&[2, 3, 48, 20], &[8, 8]).is_none());
        assert_eq!(dim(&[-1, 3, 48, -1], 2), Some(48));
        assert_eq!(dim(&[-1, 3, 48, -1], 3), None);
    }
}
