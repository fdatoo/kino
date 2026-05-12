//! Transcode handoff interface.

pub mod encoder;
pub mod job;
pub mod pipeline;
pub mod plan;

use std::{
    future::Future,
    io,
    path::{Path, PathBuf},
    pin::Pin,
    sync::Mutex,
};

pub use encoder::{
    Capabilities, DetectionConfig, Encoder, EncoderKind, EncoderRegistry, LaneId, VideoCodec,
    available_encoders,
};
pub use job::{
    JobState, JobStore, ListJobsFilter, NewJob, Scheduler, SchedulerConfig, TranscodeJob,
};
use kino_core::Id;
pub use pipeline::{
    AudioPolicy, ColorOutput, FfmpegEncodeCommand, FfmpegVmafCommand, HlsOutputSpec, InputSpec,
    LogLevel, PipelineRunner, Preset, Progress, RunOutcome, VideoFilter, VideoOutputSpec,
    verify_outputs,
};
pub use plan::VariantKind;
pub use plan::{
    ColorDowngrade, DefaultPolicy, EncodeMetadata, OutputPolicy, PlannedVariant, SampleMeasurement,
    SourceContext, TranscodeProfile, VideoRange, VmafSampleEncoder, VmafSamplingConfig,
    VmafTrialEncodeRequest, fit_crf, measure_sample_crfs, select_samples,
};

/// Errors produced by `kino-transcode`.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Encoder kind string is not recognized.
    #[error("invalid encoder kind: {0}")]
    InvalidEncoderKind(String),
    /// Job state string is not recognized.
    #[error("invalid job state: {0}")]
    InvalidJobState(String),
    /// Requested job state transition is not legal.
    #[error("invalid job state transition: {from} -> {to}")]
    InvalidTransition {
        /// Current durable job state.
        from: JobState,
        /// Requested durable job state.
        to: JobState,
    },
    /// A transcode job row was not found.
    #[error("transcode job not found: {id}")]
    JobNotFound {
        /// Missing transcode job id.
        id: Id,
    },
    /// A stored profile hash has the wrong byte length.
    #[error("invalid transcode profile hash length: {len}")]
    InvalidProfileHashLength {
        /// Actual persisted hash length.
        len: usize,
    },
    /// A stored job attempt count cannot be represented.
    #[error("invalid transcode job attempt value: {value}")]
    InvalidJobAttempt {
        /// Persisted attempt value.
        value: i64,
    },
    /// A stored job progress value cannot be represented as a percent.
    #[error("invalid transcode job progress percent: {value}")]
    InvalidJobProgress {
        /// Persisted progress value.
        value: i64,
    },
    /// A requested progress update is outside the percent range.
    #[error("invalid transcode progress percent: {pct}")]
    InvalidProgressPct {
        /// Requested progress percent.
        pct: u8,
    },
    /// A retry backoff could not be represented by Kino timestamps.
    #[error("transcode retry backoff is too large")]
    RetryBackoffTooLarge,
    /// Adding retry backoff to the current timestamp overflowed.
    #[error("transcode retry timestamp is out of range")]
    RetryTimestampOutOfRange,
    /// Encoder lane id string is not recognized.
    #[error("invalid lane id: {0}")]
    InvalidLaneId(String),
    /// Video codec string is not recognized.
    #[error("invalid video codec: {0}")]
    InvalidVideoCodec(String),
    /// Output variant kind string is not recognized.
    #[error("invalid variant kind: {0}")]
    InvalidVariantKind(String),
    /// Output color target string is not recognized.
    #[error("invalid color target: {0}")]
    InvalidColorTarget(String),
    /// Output audio policy kind string is not recognized.
    #[error("invalid audio policy kind: {0}")]
    InvalidAudioPolicyKind(String),
    /// Stored planned variant JSON could not be decoded.
    #[error("invalid planned variant json: {0}")]
    InvalidPlannedVariantJson(#[from] serde_json::Error),
    /// Internal no-op recorder state could not be accessed.
    #[error("transcode recorder lock failed: {0}")]
    RecorderLock(String),
    /// FFmpeg was cancelled and terminated before completion.
    #[error("ffmpeg killed by signal during cancellation")]
    Cancelled,
    /// Encoded output did not pass integrity checks.
    #[error("encoded output failed integrity check: {0}")]
    IntegrityFailed(String),
    /// VMAF measurement could not be produced or parsed.
    #[error("vmaf measurement failed: {0}")]
    VmafFailed(String),
    /// FFmpeg exited with a non-zero status.
    #[error("ffmpeg exited with status {status}: {stderr_tail}")]
    FfmpegFailed {
        /// Process status code, or `-1` when the process ended without a code.
        status: i32,
        /// Bounded tail of FFmpeg stderr retained for diagnostics.
        stderr_tail: String,
    },
    /// Filesystem or process I/O failed.
    #[error(transparent)]
    Io(#[from] io::Error),
    /// A database operation failed.
    #[error("transcode database operation failed: {0}")]
    Sqlx(#[from] sqlx::Error),
}

impl Error {
    /// Return whether this error is likely retryable by the scheduler.
    pub fn is_transient(&self) -> bool {
        match self {
            Self::FfmpegFailed { stderr_tail, .. } => {
                let stderr_tail = stderr_tail.to_ascii_lowercase();
                [
                    "out of memory",
                    "device is busy",
                    "resource temporarily unavailable",
                ]
                .iter()
                .any(|marker| stderr_tail.contains(marker))
            }
            Self::Io(source) => matches!(
                source.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::ResourceBusy | io::ErrorKind::TimedOut
            ),
            Self::InvalidEncoderKind(_)
            | Self::InvalidJobState(_)
            | Self::InvalidTransition { .. }
            | Self::JobNotFound { .. }
            | Self::InvalidProfileHashLength { .. }
            | Self::InvalidJobAttempt { .. }
            | Self::InvalidJobProgress { .. }
            | Self::InvalidProgressPct { .. }
            | Self::RetryBackoffTooLarge
            | Self::RetryTimestampOutOfRange
            | Self::InvalidLaneId(_)
            | Self::InvalidVideoCodec(_)
            | Self::InvalidVariantKind(_)
            | Self::InvalidColorTarget(_)
            | Self::InvalidAudioPolicyKind(_)
            | Self::InvalidPlannedVariantJson(_)
            | Self::RecorderLock(_)
            | Self::Cancelled
            | Self::IntegrityFailed(_)
            | Self::Sqlx(_)
            | Self::VmafFailed(_) => false,
        }
    }
}

/// Crate-local `Result` alias.
pub type Result<T> = std::result::Result<T, Error>;

/// Boxed future returned by transcode handoff implementations.
pub type TranscodeFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

/// Source file ready for transcode consideration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFile {
    /// Durable source-file id assigned by ingestion.
    pub id: Id,
    /// Filesystem path ingestion placed or accepted.
    pub path: PathBuf,
}

impl SourceFile {
    /// Construct a source-file handoff value.
    pub fn new(id: Id, path: impl Into<PathBuf>) -> Self {
        Self {
            id,
            path: path.into(),
        }
    }

    /// Filesystem path for this source file.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Accepted transcode handoff result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscodeReceipt {
    /// Handoff id assigned by the transcode implementation.
    pub id: Id,
    /// Source file accepted for transcode consideration.
    pub source_file: SourceFile,
    /// Human-readable action recorded by the implementation.
    pub message: String,
}

impl TranscodeReceipt {
    /// Construct a transcode handoff receipt.
    pub fn new(id: Id, source_file: SourceFile, message: impl Into<String>) -> Self {
        Self {
            id,
            source_file,
            message: message.into(),
        }
    }
}

/// Interface ingestion calls after a source file is ready.
pub trait TranscodeHandOff: Send + Sync {
    /// Submit a source file for transcode consideration.
    fn submit<'a>(&'a self, source_file: SourceFile) -> TranscodeFuture<'a, TranscodeReceipt>;
}

/// Phase 1 transcode implementation that records intent without doing work.
pub struct NoopTranscodeHandOff {
    records: Mutex<Vec<TranscodeReceipt>>,
}

impl NoopTranscodeHandOff {
    /// Construct an empty no-op transcode handoff recorder.
    pub fn new() -> Self {
        Self {
            records: Mutex::new(Vec::new()),
        }
    }

    /// Return recorded handoffs in submission order.
    pub fn records(&self) -> Result<Vec<TranscodeReceipt>> {
        self.records
            .lock()
            .map_err(lock_error)
            .map(|records| records.clone())
    }
}

impl Default for NoopTranscodeHandOff {
    fn default() -> Self {
        Self::new()
    }
}

impl TranscodeHandOff for NoopTranscodeHandOff {
    fn submit<'a>(&'a self, source_file: SourceFile) -> TranscodeFuture<'a, TranscodeReceipt> {
        Box::pin(async move {
            let receipt =
                TranscodeReceipt::new(Id::new(), source_file, "would transcode source file");
            self.records
                .lock()
                .map_err(lock_error)?
                .push(receipt.clone());

            Ok(receipt)
        })
    }
}

fn lock_error<T>(err: std::sync::PoisonError<T>) -> Error {
    Error::RecorderLock(err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn noop_records_would_transcode_receipt()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let handoff = NoopTranscodeHandOff::new();
        let source_file = SourceFile::new(Id::new(), "/library/Movie/source.mkv");

        let receipt = handoff.submit(source_file.clone()).await?;

        assert_eq!(receipt.source_file, source_file);
        assert_eq!(receipt.message, "would transcode source file");
        assert_eq!(handoff.records()?, vec![receipt]);

        Ok(())
    }
}
