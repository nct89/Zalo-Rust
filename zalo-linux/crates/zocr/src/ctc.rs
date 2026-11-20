//! CTC collapse from the Zalo OCR engine.
//!
//! Class 0 is blank. A dictionary index is `class - 1`. Repeated classes
//! collapse. The separator token, when configured, becomes a space.

/// The CTC output did not match the dictionary or contained a non-finite score.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CtcError {
    /// Steps, classes, or the dictionary length do not agree.
    #[error("CTC shape/dictionary mismatch")]
    Shape,
    /// A score was NaN or infinite.
    #[error("non-finite CTC output")]
    NonFinite,
    /// The batch tensor was not `[N,T,C]` or `[T,N,C]`.
    #[error("recognizer expected [N,T,C] or [T,N,C] output")]
    BatchShape,
    /// Widths, the padded width, or the dictionary do not agree with the tensor.
    #[error("recognizer shape/dictionary/width mismatch")]
    Width,
}

/// One collapsed symbol.
#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    /// Dictionary text, or a space when this symbol is the separator.
    pub text: String,
    /// Probability of the chosen class at the first step of the run.
    pub score: f32,
    /// First timestep of the run.
    pub start: usize,
    /// One past the last timestep of the run.
    pub end: usize,
}

/// A collapsed line.
#[derive(Debug, Clone, PartialEq)]
pub struct Decoded {
    /// Tokens joined in order.
    pub text: String,
    /// Mean token score. Zero when there are no tokens.
    pub score: f32,
    /// Collapsed symbols.
    pub tokens: Vec<Token>,
    /// Timesteps that were decoded.
    pub steps: usize,
}

/// Dictionary view for one decode.
pub struct Labels<'a> {
    /// Symbols. Class `i + 1` reads `dictionary[i]`.
    pub dictionary: &'a [String],
    /// Dictionary symbol that should be rendered as a space. The vault uses `"|"`.
    pub separator: Option<&'a str>,
    /// When set, one flag per class including blank. `false` classes are ignored.
    pub allowed: Option<&'a [bool]>,
}

/// Collapse one CTC row-major matrix of `steps * classes` values.
pub fn decode(
    data: &[f32],
    steps: usize,
    classes: usize,
    labels: &Labels<'_>,
) -> Result<Decoded, CtcError> {
    let dictionary = labels.dictionary;
    if classes != dictionary.len() + 1
        || steps.checked_mul(classes) != Some(data.len())
        || steps == 0
        || labels
            .allowed
            .is_some_and(|allowed| allowed.len() != classes)
    {
        return Err(CtcError::Shape);
    }
    let allowed = |class: usize| labels.allowed.is_none_or(|flags| flags[class]);
    let mut tokens = Vec::new();
    let mut prev = 0usize;
    for (time, row) in data.chunks_exact(classes).enumerate() {
        if row.iter().any(|value| !value.is_finite()) {
            return Err(CtcError::NonFinite);
        }
        let class = row
            .iter()
            .enumerate()
            .filter(|(class, _)| allowed(*class))
            .max_by(|left, right| left.1.total_cmp(right.1))
            .unwrap()
            .0;
        let kept = || {
            row.iter()
                .enumerate()
                .filter(|(class, _)| allowed(*class))
                .map(|(_, value)| *value)
        };
        let sum: f32 = row.iter().sum();
        let probability =
            if row.iter().all(|value| (0.0..=1.0).contains(value)) && (sum - 1.0).abs() < 0.01 {
                row[class] / kept().sum::<f32>().max(f32::MIN_POSITIVE)
            } else {
                let max = row[class];
                1.0 / kept().map(|value| (value - max).exp()).sum::<f32>()
            };
        if class != 0 {
            if class != prev {
                let symbol = dictionary[class - 1].as_str();
                let text = if labels.separator == Some(symbol) {
                    " ".to_owned()
                } else {
                    symbol.to_owned()
                };
                tokens.push(Token {
                    text,
                    score: probability,
                    start: time,
                    end: time + 1,
                });
            } else if let Some(last) = tokens.last_mut() {
                last.end = time + 1;
            }
        }
        prev = class;
    }
    let text = tokens
        .iter()
        .map(|token| token.text.as_str())
        .collect::<String>();
    let score = if tokens.is_empty() {
        0.0
    } else {
        tokens.iter().map(|token| token.score).sum::<f32>() / tokens.len() as f32
    };
    Ok(Decoded {
        text,
        score,
        tokens,
        steps,
    })
}

/// Decode a recognizer batch.
///
/// `shape` is `[N,T,C]`, `[T,N,C]`, or `[T,C]` when `batch` is 1. Padded
/// timesteps past each item's width are dropped before [`decode`].
pub fn decode_batch(
    data: &[f32],
    shape: &[i64],
    batch: usize,
    widths: &[u32],
    padded_width: u32,
    labels: &Labels<'_>,
) -> Result<Vec<Decoded>, CtcError> {
    let dictionary = labels.dictionary;
    let (steps, classes, time_first) = match shape {
        [n, t, c] if *n == batch as i64 && *t > 0 && *c > 0 => (*t as usize, *c as usize, false),
        [t, n, c] if *n == batch as i64 && *t > 0 && *c > 0 => (*t as usize, *c as usize, true),
        [t, c] if batch == 1 && *t > 0 && *c > 0 => (*t as usize, *c as usize, false),
        _ => return Err(CtcError::BatchShape),
    };
    if batch == 0
        || widths.len() != batch
        || padded_width == 0
        || classes != dictionary.len() + 1
        || steps
            .checked_mul(batch)
            .and_then(|count| count.checked_mul(classes))
            != Some(data.len())
        || widths
            .iter()
            .any(|width| *width == 0 || *width > padded_width)
    {
        return Err(CtcError::Width);
    }
    let mut decoded = Vec::with_capacity(batch);
    for (item, width) in widths.iter().enumerate() {
        let valid_steps = ((steps as u64 * u64::from(*width)).div_ceil(u64::from(padded_width))
            as usize)
            .min(steps);
        let mut sequence = Vec::with_capacity(valid_steps * classes);
        for step in 0..valid_steps {
            let offset = if time_first {
                (step * batch + item) * classes
            } else {
                (item * steps + step) * classes
            };
            sequence.extend_from_slice(&data[offset..offset + classes]);
        }
        decoded.push(decode(&sequence, valid_steps, classes, labels)?);
    }
    Ok(decoded)
}

/// One word split on whitespace tokens. The space stays at the end of the word
/// so joining the words rebuilds the line.
#[derive(Debug, Clone, PartialEq)]
pub struct Word {
    /// Word text, including a trailing separator when the line had one.
    pub text: String,
    /// Mean score of the tokens in the word.
    pub score: f32,
    /// Start of the word as a fraction of the line.
    pub start: f64,
    /// End of the word as a fraction of the line.
    pub end: f64,
    /// Start of the non-space ink, as a fraction of the line.
    pub core_start: f64,
    /// End of the non-space ink, as a fraction of the line.
    pub core_end: f64,
}

/// Split a line on whitespace tokens.
pub fn words(decoded: &Decoded) -> Vec<Word> {
    let mut words = Vec::new();
    let mut text = String::new();
    let mut scores = Vec::new();
    let mut start = 0usize;
    let mut end = 0usize;
    let mut core: Option<(usize, usize)> = None;
    let steps = decoded.steps as f64;
    let flush = |words: &mut Vec<Word>,
                 text: &mut String,
                 scores: &mut Vec<f32>,
                 start: usize,
                 end: usize,
                 core: &mut Option<(usize, usize)>| {
        if text.is_empty() {
            return;
        }
        let (core_start, core_end) = core.take().unwrap_or((start, end));
        let score = scores.iter().sum::<f32>() / scores.len() as f32;
        words.push(Word {
            text: std::mem::take(text),
            score,
            start: start as f64 / steps,
            end: end as f64 / steps,
            core_start: core_start as f64 / steps,
            core_end: core_end as f64 / steps,
        });
        scores.clear();
    };
    for (index, token) in decoded.tokens.iter().enumerate() {
        if text.is_empty() {
            start = token.start;
        }
        text.push_str(&token.text);
        scores.push(token.score);
        end = token.end;
        let space = token.text.chars().all(char::is_whitespace);
        if !space {
            core = Some(core.map_or((token.start, token.end), |(core_start, _)| {
                (core_start, token.end)
            }));
        }
        if space
            && decoded
                .tokens
                .get(index + 1)
                .is_none_or(|next| !next.text.chars().all(char::is_whitespace))
        {
            flush(&mut words, &mut text, &mut scores, start, end, &mut core);
        }
    }
    flush(&mut words, &mut text, &mut scores, start, end, &mut core);
    words
}

/// Keep a line when filtering is off, recognition is high, or both scores clear their floors.
pub fn keep_line(
    enabled: bool,
    detection: f32,
    recognition: f32,
    det_min: f32,
    rec_min: f32,
    rec_keep: f32,
) -> bool {
    !enabled || recognition >= rec_keep || (recognition >= rec_min && detection >= det_min)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_classes_collapse_around_a_blank() {
        let dictionary = vec!["a".to_owned()];
        let data = [
            0.0, 1.0, // a
            0.0, 1.0, // same run
            1.0, 0.0, // blank
            0.0, 1.0, // new a
        ];
        let decoded = decode(
            &data,
            4,
            2,
            &Labels {
                dictionary: &dictionary,
                separator: None,
                allowed: None,
            },
        )
        .unwrap();
        assert_eq!(decoded.text, "aa");
        assert_eq!(decoded.tokens.len(), 2);
        assert_eq!(decoded.tokens[0].end, 2);
        assert_eq!(decoded.tokens[1].start, 3);
    }

    #[test]
    fn model_separator_becomes_space_in_text_and_words() {
        let dictionary = vec!["xin".into(), "|".into(), "chào!".into()];
        let data = [0., 1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1.];
        let decoded = decode(
            &data,
            3,
            4,
            &Labels {
                dictionary: &dictionary,
                separator: Some("|"),
                allowed: None,
            },
        )
        .unwrap();
        assert_eq!(decoded.text, "xin chào!");
        let words = words(&decoded);
        assert_eq!(
            words
                .iter()
                .map(|word| word.text.as_str())
                .collect::<Vec<_>>(),
            ["xin ", "chào!"]
        );
        assert_eq!(
            words
                .iter()
                .map(|word| word.text.as_str())
                .collect::<String>(),
            decoded.text
        );
    }

    #[test]
    fn disallowed_class_is_never_picked() {
        let dictionary = vec!["a".into(), "\u{307}".into()];
        let data = [0.1, 0.8, 0.1, 0.1, 0.3, 0.6];
        let allowed = [true, true, false];
        let decoded = decode(
            &data,
            2,
            3,
            &Labels {
                dictionary: &dictionary,
                separator: None,
                allowed: Some(&allowed),
            },
        )
        .unwrap();
        assert_eq!(decoded.text, "a");
        let dictionary = vec!["|".into()];
        let decoded = decode(
            &[0.1, 0.9],
            1,
            2,
            &Labels {
                dictionary: &dictionary,
                separator: None,
                allowed: None,
            },
        )
        .unwrap();
        assert_eq!(decoded.text, "|");
    }

    #[test]
    fn non_finite_output_is_rejected() {
        let dictionary = vec!["a".into()];
        let error = decode(
            &[0.0, f32::NAN],
            1,
            2,
            &Labels {
                dictionary: &dictionary,
                separator: None,
                allowed: None,
            },
        )
        .unwrap_err();
        assert_eq!(error, CtcError::NonFinite);
    }
}
