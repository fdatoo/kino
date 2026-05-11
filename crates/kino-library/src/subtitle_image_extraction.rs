//! Image subtitle extraction interfaces.

use std::{future::Future, path::PathBuf, pin::Pin, time::Duration};

use crate::Result;

/// Boxed future returned by image subtitle extraction implementations.
pub type ImageSubtitleExtractionFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Vec<ImageSubtitleFrame>>> + Send + 'a>>;

/// Timestamped raster frame extracted from an image subtitle track.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageSubtitleFrame {
    /// Cue start time relative to the media timeline.
    pub start: Duration,
    /// Cue end time relative to the media timeline.
    pub end: Duration,
    /// Filesystem path to the extracted frame image.
    pub image_path: PathBuf,
}

impl ImageSubtitleFrame {
    /// Construct an extracted image subtitle frame.
    pub fn new(start: Duration, end: Duration, image_path: impl Into<PathBuf>) -> Self {
        Self {
            start,
            end,
            image_path: image_path.into(),
        }
    }
}

/// Extractor for one image subtitle track.
pub trait ImageSubtitleExtraction: Send + Sync {
    /// Extract raster frames for a single image subtitle track.
    fn extract_frames<'a>(&'a self) -> ImageSubtitleExtractionFuture<'a>;
}
