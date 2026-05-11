//! Source-file ingestion handoff.

use std::path::PathBuf;

use kino_core::Id;
use kino_library::{
    CanonicalLayoutInput, CanonicalLayoutResult, CanonicalLayoutWriter, CanonicalMediaTarget,
};
use kino_transcode::{SourceFile, TranscodeHandOff, TranscodeReceipt};

use crate::{Error, Result};

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

#[cfg(test)]
mod tests {
    use super::*;
    use kino_core::CanonicalLayoutTransfer;
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
}
