//! Source-file ingestion handoff.

use std::{collections::HashSet, fmt, path::PathBuf};

use kino_core::{CanonicalIdentityId, Id, ProbeResult};
use kino_library::{
    CanonicalLayoutInput, CanonicalLayoutResult, CanonicalLayoutWriter, CanonicalMediaTarget,
};
use kino_transcode::{SourceFile, TranscodeHandOff, TranscodeReceipt};

use crate::{Error, Result};

/// Percentage of expected runtime accepted when matching probed files.
pub const PROBED_FILE_DURATION_TOLERANCE_PERCENT: u32 = 20;
/// Minimum runtime tolerance in seconds when matching probed files.
pub const PROBED_FILE_DURATION_MIN_TOLERANCE_SECONDS: u32 = 300;

/// Minimal ingestion pipeline entry point for a ready source file.
pub struct IngestionPipeline<T> {
    transcode: T,
    canonical_layout: Option<CanonicalLayoutWriter>,
}

impl<T> IngestionPipeline<T> {
    /// Construct an ingestion pipeline with a transcode handoff implementation.
    pub const fn new(transcode: T) -> Self {
        Self {
            transcode,
            canonical_layout: None,
        }
    }

    /// Construct an ingestion pipeline with canonical layout placement.
    pub fn with_canonical_layout(transcode: T, canonical_layout: CanonicalLayoutWriter) -> Self {
        Self {
            transcode,
            canonical_layout: Some(canonical_layout),
        }
    }

    /// Return the configured transcode handoff implementation.
    pub const fn transcode(&self) -> &T {
        &self.transcode
    }
}

impl<T> IngestionPipeline<T>
where
    T: TranscodeHandOff,
{
    /// Ingest a ready source file and hand it to transcode.
    pub async fn ingest_source_file(&self, input: IngestSourceFile) -> Result<IngestedSourceFile> {
        let source_file = SourceFile::new(input.source_file_id, input.source_path);
        let transcode = self.transcode.submit(source_file.clone()).await?;

        Ok(IngestedSourceFile {
            source_file,
            transcode,
        })
    }

    /// Place a source file in the canonical layout and hand it to transcode.
    pub async fn ingest_canonical_source_file(
        &self,
        input: IngestCanonicalSourceFile,
    ) -> Result<IngestedCanonicalSourceFile> {
        let canonical_layout = self
            .canonical_layout
            .as_ref()
            .ok_or(Error::CanonicalLayoutNotConfigured)?;
        let layout = canonical_layout
            .place(CanonicalLayoutInput::new(input.source_path, input.target))
            .await?;
        let source_file = SourceFile::new(input.source_file_id, layout.canonical_path.clone());
        let transcode = self.transcode.submit(source_file.clone()).await?;

        Ok(IngestedCanonicalSourceFile {
            layout,
            source_file,
            transcode,
        })
    }
}

/// Input for ingesting a source file that already exists on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngestSourceFile {
    /// Source-file id assigned by ingestion.
    pub source_file_id: Id,
    /// Path to the source file ready for transcode consideration.
    pub source_path: PathBuf,
}

impl IngestSourceFile {
    /// Construct an ingest input with a new source-file id.
    pub fn new(source_path: impl Into<PathBuf>) -> Self {
        Self {
            source_file_id: Id::new(),
            source_path: source_path.into(),
        }
    }

    /// Construct an ingest input with an explicit source-file id.
    pub fn with_id(source_file_id: Id, source_path: impl Into<PathBuf>) -> Self {
        Self {
            source_file_id,
            source_path: source_path.into(),
        }
    }
}

/// Input for ingesting a source file into the canonical library layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngestCanonicalSourceFile {
    /// Source-file id assigned by ingestion.
    pub source_file_id: Id,
    /// Source file accepted by ingestion.
    pub source_path: PathBuf,
    /// Canonical media target that determines the library path.
    pub target: CanonicalMediaTarget,
}

impl IngestCanonicalSourceFile {
    /// Construct canonical ingest input with a new source-file id.
    pub fn new(source_path: impl Into<PathBuf>, target: CanonicalMediaTarget) -> Self {
        Self {
            source_file_id: Id::new(),
            source_path: source_path.into(),
            target,
        }
    }

    /// Construct canonical ingest input with an explicit source-file id.
    pub fn with_id(
        source_file_id: Id,
        source_path: impl Into<PathBuf>,
        target: CanonicalMediaTarget,
    ) -> Self {
        Self {
            source_file_id,
            source_path: source_path.into(),
            target,
        }
    }
}

/// Result of source-file ingestion and transcode handoff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngestedSourceFile {
    /// Source file accepted by ingestion.
    pub source_file: SourceFile,
    /// Transcode handoff receipt.
    pub transcode: TranscodeReceipt,
}

/// Result of canonical source-file ingestion and transcode handoff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngestedCanonicalSourceFile {
    /// Canonical layout placement result.
    pub layout: CanonicalLayoutResult,
    /// Canonical source file accepted by ingestion.
    pub source_file: SourceFile,
    /// Transcode handoff receipt.
    pub transcode: TranscodeReceipt,
}

/// Request-side facts used to verify a probed provider file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedProbedFile {
    /// Canonical identity this file is expected to satisfy.
    pub canonical_identity_id: CanonicalIdentityId,
    /// Expected display title, when known.
    pub title: Option<String>,
    /// Expected runtime in seconds, when known.
    pub runtime_seconds: Option<u32>,
    /// Audio languages explicitly required by the request.
    pub required_audio_languages: Vec<String>,
    /// Subtitle languages explicitly required by the request.
    pub required_subtitle_languages: Vec<String>,
}

impl ExpectedProbedFile {
    /// Construct expected probe facts for a canonical identity.
    pub fn new(canonical_identity_id: CanonicalIdentityId) -> Self {
        Self {
            canonical_identity_id,
            title: None,
            runtime_seconds: None,
            required_audio_languages: Vec::new(),
            required_subtitle_languages: Vec::new(),
        }
    }

    /// Set the expected display title.
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// Set the expected runtime in seconds.
    pub const fn with_runtime_seconds(mut self, runtime_seconds: u32) -> Self {
        self.runtime_seconds = Some(runtime_seconds);
        self
    }

    /// Set required audio languages.
    pub fn with_required_audio_languages<I, S>(mut self, languages: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.required_audio_languages = languages.into_iter().map(Into::into).collect();
        self
    }

    /// Set required subtitle languages.
    pub fn with_required_subtitle_languages<I, S>(mut self, languages: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.required_subtitle_languages = languages.into_iter().map(Into::into).collect();
        self
    }
}

/// Probe output used to verify a provider file.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProbedFile {
    /// Detected title, when the probe could identify one.
    pub title: Option<String>,
    /// Detected duration in seconds, when available.
    pub duration_seconds: Option<u32>,
    /// Audio languages detected in the file.
    pub audio_languages: Vec<String>,
    /// Subtitle languages detected in the file.
    pub subtitle_languages: Vec<String>,
}

impl ProbedFile {
    /// Construct an empty probed-file projection.
    pub fn new() -> Self {
        Self::default()
    }

    /// Project a rich file-probe result into request-matching facts.
    pub fn from_probe_result(probe: &ProbeResult) -> Self {
        Self {
            title: probe.title.clone(),
            duration_seconds: probe
                .duration
                .and_then(|duration| u32::try_from(duration.as_secs()).ok()),
            audio_languages: probe
                .audio_streams
                .iter()
                .filter_map(|stream| stream.language.clone())
                .collect(),
            subtitle_languages: probe
                .subtitle_streams
                .iter()
                .filter_map(|stream| stream.language.clone())
                .collect(),
        }
    }

    /// Set the detected title.
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// Set the detected duration in seconds.
    pub const fn with_duration_seconds(mut self, duration_seconds: u32) -> Self {
        self.duration_seconds = Some(duration_seconds);
        self
    }

    /// Set detected audio languages.
    pub fn with_audio_languages<I, S>(mut self, languages: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.audio_languages = languages.into_iter().map(Into::into).collect();
        self
    }

    /// Set detected subtitle languages.
    pub fn with_subtitle_languages<I, S>(mut self, languages: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.subtitle_languages = languages.into_iter().map(Into::into).collect();
        self
    }
}

/// Mismatch found while verifying a probed file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbedFileMismatch {
    /// Expected a title but the probe did not identify one.
    MissingTitle {
        /// Expected display title.
        expected: String,
    },

    /// The detected title does not match the expected title.
    WrongTitle {
        /// Expected display title.
        expected: String,
        /// Detected title.
        actual: String,
    },

    /// Expected a duration but the probe did not identify one.
    MissingDuration {
        /// Expected runtime in seconds.
        expected_seconds: u32,
    },

    /// The detected duration is outside the accepted tolerance.
    DurationMismatch {
        /// Expected runtime in seconds.
        expected_seconds: u32,
        /// Detected duration in seconds.
        actual_seconds: u32,
        /// Accepted absolute tolerance in seconds.
        tolerance_seconds: u32,
    },

    /// A required audio language was not detected.
    MissingAudioLanguage {
        /// Required language code.
        language: String,
    },

    /// A required subtitle language was not detected.
    MissingSubtitleLanguage {
        /// Required language code.
        language: String,
    },
}

impl fmt::Display for ProbedFileMismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingTitle { expected } => write!(f, "missing title expected {expected:?}"),
            Self::WrongTitle { expected, actual } => {
                write!(f, "title mismatch expected {expected:?} got {actual:?}")
            }
            Self::MissingDuration { expected_seconds } => {
                write!(f, "missing duration expected {expected_seconds}s")
            }
            Self::DurationMismatch {
                expected_seconds,
                actual_seconds,
                tolerance_seconds,
            } => write!(
                f,
                "duration mismatch expected {expected_seconds}s got {actual_seconds}s tolerance {tolerance_seconds}s"
            ),
            Self::MissingAudioLanguage { language } => {
                write!(f, "missing audio language {language}")
            }
            Self::MissingSubtitleLanguage { language } => {
                write!(f, "missing subtitle language {language}")
            }
        }
    }
}

/// Result of matching a probed file against request-side expectations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbedFileMatch {
    /// Mismatches found during verification.
    pub mismatches: Vec<ProbedFileMismatch>,
}

impl ProbedFileMatch {
    /// Whether the probed file satisfies all provided expectations.
    pub fn is_match(&self) -> bool {
        self.mismatches.is_empty()
    }

    /// Human-readable mismatch summary.
    pub fn summary(&self) -> String {
        if self.mismatches.is_empty() {
            return "probed file matched request".to_owned();
        }

        self.mismatches
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ")
    }
}

/// Verify probe output against request-side expectations.
pub fn match_probed_file(expected: &ExpectedProbedFile, probed: &ProbedFile) -> ProbedFileMatch {
    let mut mismatches = Vec::new();

    if let Some(expected_title) = &expected.title {
        match &probed.title {
            Some(actual_title) => {
                if normalize_title(expected_title) != normalize_title(actual_title) {
                    mismatches.push(ProbedFileMismatch::WrongTitle {
                        expected: expected_title.clone(),
                        actual: actual_title.clone(),
                    });
                }
            }
            None => mismatches.push(ProbedFileMismatch::MissingTitle {
                expected: expected_title.clone(),
            }),
        }
    }

    if let Some(expected_seconds) = expected.runtime_seconds {
        match probed.duration_seconds {
            Some(actual_seconds) => {
                let tolerance_seconds = duration_tolerance_seconds(expected_seconds);
                let delta = expected_seconds.abs_diff(actual_seconds);
                if delta > tolerance_seconds {
                    mismatches.push(ProbedFileMismatch::DurationMismatch {
                        expected_seconds,
                        actual_seconds,
                        tolerance_seconds,
                    });
                }
            }
            None => mismatches.push(ProbedFileMismatch::MissingDuration { expected_seconds }),
        }
    }

    let audio_languages = normalized_language_set(&probed.audio_languages);
    for language in normalized_languages(&expected.required_audio_languages) {
        if !audio_languages.contains(&language) {
            mismatches.push(ProbedFileMismatch::MissingAudioLanguage { language });
        }
    }

    let subtitle_languages = normalized_language_set(&probed.subtitle_languages);
    for language in normalized_languages(&expected.required_subtitle_languages) {
        if !subtitle_languages.contains(&language) {
            mismatches.push(ProbedFileMismatch::MissingSubtitleLanguage { language });
        }
    }

    ProbedFileMatch { mismatches }
}

fn duration_tolerance_seconds(expected_seconds: u32) -> u32 {
    let percent = expected_seconds.saturating_mul(PROBED_FILE_DURATION_TOLERANCE_PERCENT) / 100;
    percent.max(PROBED_FILE_DURATION_MIN_TOLERANCE_SECONDS)
}

fn normalize_title(title: &str) -> String {
    let mut normalized = String::new();
    let mut previous_was_space = false;

    for ch in title.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            normalized.push(ch.to_ascii_lowercase());
            previous_was_space = false;
        } else if !previous_was_space {
            normalized.push(' ');
            previous_was_space = true;
        }
    }

    normalized.trim().to_owned()
}

fn normalized_language_set(languages: &[String]) -> HashSet<String> {
    normalized_languages(languages).collect()
}

fn normalized_languages(languages: &[String]) -> impl Iterator<Item = String> + '_ {
    languages.iter().filter_map(|language| {
        let language = language.trim().to_ascii_lowercase();
        (!language.is_empty()).then_some(language)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use kino_core::{CanonicalIdentityId, CanonicalLayoutTransfer, TmdbId};
    use kino_transcode::NoopTranscodeHandOff;

    #[tokio::test]
    async fn ingestion_hands_source_file_to_noop_transcode()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let transcode = NoopTranscodeHandOff::new();
        let pipeline = IngestionPipeline::new(transcode);
        let source_file_id = Id::new();
        let input = IngestSourceFile::with_id(source_file_id, "/library/movie/source.mkv");

        let ingested = pipeline.ingest_source_file(input).await?;

        assert_eq!(ingested.source_file.id, source_file_id);
        assert_eq!(
            ingested.source_file.path,
            PathBuf::from("/library/movie/source.mkv")
        );
        assert_eq!(ingested.transcode.message, "would transcode source file");
        assert_eq!(pipeline.transcode().records()?, vec![ingested.transcode]);

        Ok(())
    }

    #[tokio::test]
    async fn canonical_ingestion_places_file_before_transcode()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let library_root = tempfile::tempdir()?;
        let source = library_root.path().join("source.mkv");
        tokio::fs::write(&source, b"movie bytes").await?;
        let transcode = NoopTranscodeHandOff::new();
        let layout =
            CanonicalLayoutWriter::new(library_root.path(), CanonicalLayoutTransfer::HardLink);
        let pipeline = IngestionPipeline::with_canonical_layout(transcode, layout);
        let source_file_id = Id::new();

        let ingested = pipeline
            .ingest_canonical_source_file(IngestCanonicalSourceFile::with_id(
                source_file_id,
                &source,
                CanonicalMediaTarget::movie("Moon", 2009),
            ))
            .await?;
        let expected = library_root
            .path()
            .join("Movies")
            .join("Moon (2009)")
            .join("Moon (2009).mkv");

        assert_eq!(ingested.layout.canonical_path, expected);
        assert_eq!(ingested.source_file.id, source_file_id);
        assert_eq!(ingested.source_file.path, expected);
        assert_eq!(
            ingested.transcode.source_file.path,
            ingested.layout.canonical_path
        );
        assert_eq!(tokio::fs::read(&source).await?, b"movie bytes");
        assert_eq!(
            tokio::fs::read(&ingested.layout.canonical_path).await?,
            b"movie bytes"
        );

        Ok(())
    }

    #[test]
    fn match_probed_file_reports_title_duration_and_language_mismatches() {
        let expected = ExpectedProbedFile::new(identity(550))
            .with_title("The Matrix")
            .with_runtime_seconds(8160)
            .with_required_audio_languages(["eng"])
            .with_required_subtitle_languages(["spa"]);
        let probed = ProbedFile::new()
            .with_title("Toy Story")
            .with_duration_seconds(1200)
            .with_audio_languages(["jpn"])
            .with_subtitle_languages(["eng"]);

        let result = match_probed_file(&expected, &probed);

        assert_eq!(
            result.mismatches,
            vec![
                ProbedFileMismatch::WrongTitle {
                    expected: String::from("The Matrix"),
                    actual: String::from("Toy Story"),
                },
                ProbedFileMismatch::DurationMismatch {
                    expected_seconds: 8160,
                    actual_seconds: 1200,
                    tolerance_seconds: 1632,
                },
                ProbedFileMismatch::MissingAudioLanguage {
                    language: String::from("eng"),
                },
                ProbedFileMismatch::MissingSubtitleLanguage {
                    language: String::from("spa"),
                },
            ]
        );
    }

    #[test]
    fn match_probed_file_accepts_normalized_title_and_close_duration() {
        let expected = ExpectedProbedFile::new(identity(550))
            .with_title("Spider-Man: Into the Spider-Verse")
            .with_runtime_seconds(7000)
            .with_required_audio_languages(["ENG"]);
        let probed = ProbedFile::new()
            .with_title("spider man into the spider verse")
            .with_duration_seconds(7600)
            .with_audio_languages(["eng", "jpn"]);

        let result = match_probed_file(&expected, &probed);

        assert!(result.is_match(), "got: {}", result.summary());
    }

    fn identity(tmdb_id: u32) -> CanonicalIdentityId {
        let Some(tmdb_id) = TmdbId::new(tmdb_id) else {
            panic!("test tmdb id must be positive");
        };
        CanonicalIdentityId::tmdb_movie(tmdb_id)
    }
}
