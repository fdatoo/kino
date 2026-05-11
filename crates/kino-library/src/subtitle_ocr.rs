//! OCR support for extracted image subtitle frames.

use std::{
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

use crate::{Result, subtitle_image_extraction::ImageSubtitleFrame};

/// OCR engine for one extracted image subtitle frame.
pub trait OcrEngine: Send + Sync {
    /// Run OCR for the image at `image_path`.
    fn ocr(&self, image_path: &Path) -> Result<OcrFrameResult>;
}

/// Tesseract-backed OCR engine invoked as a local process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TesseractOcrEngine {
    language: String,
    binary_path: PathBuf,
}

impl TesseractOcrEngine {
    /// Construct a Tesseract OCR engine from a language code and binary path.
    pub fn new(language: impl Into<String>, binary_path: impl Into<PathBuf>) -> Self {
        Self {
            language: language.into(),
            binary_path: binary_path.into(),
        }
    }

    /// Construct a Tesseract OCR engine from Kino OCR configuration.
    pub fn from_config(config: &kino_core::config::OcrConfig) -> Self {
        Self::new(config.language.clone(), config.tesseract_path.clone())
    }

    /// Construct a Tesseract OCR engine from environment defaults.
    pub fn from_env() -> Self {
        let language = std::env::var("KINO_OCR__LANGUAGE").unwrap_or_else(|_| String::from("eng"));
        let binary_path = std::env::var_os("KINO_OCR__TESSERACT_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("tesseract"));
        Self::new(language, binary_path)
    }
}

impl Default for TesseractOcrEngine {
    fn default() -> Self {
        Self::from_env()
    }
}

impl OcrEngine for TesseractOcrEngine {
    fn ocr(&self, image_path: &Path) -> Result<OcrFrameResult> {
        let output = Command::new(&self.binary_path)
            .arg(image_path)
            .arg("stdout")
            .arg("-l")
            .arg(&self.language)
            .arg("tsv")
            .output()
            .map_err(|source| crate::Error::OcrCommandIo {
                binary_path: self.binary_path.clone(),
                source,
            })?;

        if !output.status.success() {
            return Err(crate::Error::OcrCommandFailed {
                binary_path: self.binary_path.clone(),
                status: output.status.code().map_or_else(
                    || String::from("terminated by signal"),
                    |code| code.to_string(),
                ),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            });
        }

        let stdout = String::from_utf8(output.stdout)?;
        parse_tesseract_tsv(&stdout)
    }
}

/// OCR result for one frame.
#[derive(Debug, Clone, PartialEq)]
pub struct OcrFrameResult {
    /// Recognized text. Empty text is valid when OCR finds no words.
    pub text: String,
    /// Average positive word confidence for the frame.
    pub avg_confidence: f32,
}

/// Time-coded OCR cue derived from an image subtitle frame.
#[derive(Debug, Clone, PartialEq)]
pub struct OcrCue {
    /// Cue start time relative to the media timeline.
    pub start: Duration,
    /// Cue end time relative to the media timeline.
    pub end: Duration,
    /// Recognized cue text. Empty text preserves timeline alignment.
    pub text: String,
    /// Average positive word confidence for this cue.
    pub confidence: f32,
}

/// Run OCR across extracted image subtitle frames and preserve cue timing.
pub async fn ocr_subtitle_track(
    engine: &dyn OcrEngine,
    frames: &[ImageSubtitleFrame],
) -> Result<Vec<OcrCue>> {
    let mut cues = Vec::with_capacity(frames.len());

    for frame in frames {
        let result = engine.ocr(&frame.image_path)?;
        cues.push(OcrCue {
            start: frame.start,
            end: frame.end,
            text: result.text,
            confidence: result.avg_confidence,
        });
    }

    Ok(cues)
}

#[derive(Debug, Clone, PartialEq)]
struct TesseractWord {
    line_key: (u32, u32, u32, u32),
    text: String,
    confidence: f32,
}

fn parse_tesseract_tsv(tsv: &str) -> Result<OcrFrameResult> {
    let mut lines = tsv.lines();
    let Some(header) = lines.next() else {
        return Err(crate::Error::InvalidOcrTsv {
            reason: String::from("missing header"),
        });
    };

    if header.split('\t').count() < 12 {
        return Err(crate::Error::InvalidOcrTsv {
            reason: String::from("header has fewer than 12 columns"),
        });
    }

    let mut words = Vec::new();
    for line in lines.filter(|line| !line.trim().is_empty()) {
        let columns = line.splitn(12, '\t').collect::<Vec<_>>();
        if columns.len() != 12 {
            return Err(crate::Error::InvalidOcrTsv {
                reason: String::from("row has fewer than 12 columns"),
            });
        }

        let word_num = parse_u32(columns[5], "word_num")?;
        let text = columns[11].trim();
        if word_num == 0 || text.is_empty() {
            continue;
        }

        words.push(TesseractWord {
            line_key: (
                parse_u32(columns[1], "page_num")?,
                parse_u32(columns[2], "block_num")?,
                parse_u32(columns[3], "par_num")?,
                parse_u32(columns[4], "line_num")?,
            ),
            text: text.to_owned(),
            confidence: parse_f32(columns[10], "conf")?,
        });
    }

    Ok(OcrFrameResult {
        text: join_words_by_line(&words),
        avg_confidence: average_positive_confidence(words.iter().map(|word| word.confidence)),
    })
}

fn join_words_by_line(words: &[TesseractWord]) -> String {
    let mut text = String::new();
    let mut previous_key = None;

    for word in words {
        match previous_key {
            Some(key) if key == word.line_key => text.push(' '),
            Some(_) => text.push('\n'),
            None => {}
        }
        text.push_str(&word.text);
        previous_key = Some(word.line_key);
    }

    text
}

fn average_positive_confidence(confidences: impl IntoIterator<Item = f32>) -> f32 {
    let mut total = 0.0;
    let mut count = 0_u32;

    for confidence in confidences {
        if confidence > 0.0 {
            total += confidence;
            count += 1;
        }
    }

    if count == 0 {
        0.0
    } else {
        total / count as f32
    }
}

fn parse_u32(value: &str, field: &'static str) -> Result<u32> {
    value.parse().map_err(
        |source: std::num::ParseIntError| crate::Error::InvalidOcrTsvField {
            field,
            value: value.to_owned(),
            reason: source.to_string(),
        },
    )
}

fn parse_f32(value: &str, field: &'static str) -> Result<f32> {
    value.parse().map_err(
        |source: std::num::ParseFloatError| crate::Error::InvalidOcrTsvField {
            field,
            value: value.to_owned(),
            reason: source.to_string(),
        },
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::subtitle_image_extraction::{
        ImageSubtitleExtraction, ImageSubtitleExtractionFuture,
    };

    #[test]
    fn parses_tesseract_tsv_words_and_confidence() -> Result<()> {
        let tsv = "\
level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext
1\t1\t0\t0\t0\t0\t0\t0\t100\t40\t-1\t
5\t1\t1\t1\t1\t1\t10\t10\t20\t10\t91.5\tHello
5\t1\t1\t1\t1\t2\t35\t10\t20\t10\t82.5\tKino
5\t1\t1\t1\t2\t1\t10\t25\t20\t10\t0.0\tignored
";

        let result = parse_tesseract_tsv(tsv)?;

        assert_eq!(result.text, "Hello Kino\nignored");
        assert_eq!(result.avg_confidence, 87.0);
        Ok(())
    }

    #[test]
    fn aggregates_only_positive_confidence_values() {
        let confidence = average_positive_confidence([90.0, -1.0, 0.0, 30.0]);

        assert_eq!(confidence, 60.0);
    }

    #[tokio::test]
    async fn maps_extracted_frames_to_ocr_cues() -> Result<()> {
        struct FakeExtraction {
            image_path: PathBuf,
        }

        impl ImageSubtitleExtraction for FakeExtraction {
            fn extract_frames<'a>(&'a self) -> ImageSubtitleExtractionFuture<'a> {
                Box::pin(async move {
                    Ok(vec![ImageSubtitleFrame::new(
                        Duration::from_secs(1),
                        Duration::from_secs(3),
                        self.image_path.clone(),
                    )])
                })
            }
        }

        struct FakeEngine;

        impl OcrEngine for FakeEngine {
            fn ocr(&self, _image_path: &Path) -> Result<OcrFrameResult> {
                Ok(OcrFrameResult {
                    text: String::from("HELLO KINO"),
                    avg_confidence: 96.0,
                })
            }
        }

        let extraction = FakeExtraction {
            image_path: PathBuf::from("frame.png"),
        };
        let frames = extraction.extract_frames().await?;
        let cues = ocr_subtitle_track(&FakeEngine, &frames).await?;

        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].start, Duration::from_secs(1));
        assert_eq!(cues[0].end, Duration::from_secs(3));
        assert_eq!(cues[0].text, "HELLO KINO");
        assert_eq!(cues[0].confidence, 96.0);
        Ok(())
    }
}
