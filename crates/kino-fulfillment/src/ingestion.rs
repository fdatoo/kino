//! Source-file ingestion handoff.

use std::path::PathBuf;

use kino_core::Id;
use kino_transcode::{SourceFile, TranscodeHandOff, TranscodeReceipt};

use crate::Result;

/// Minimal ingestion pipeline entry point for a ready source file.
pub struct IngestionPipeline<T> {
    transcode: T,
}

impl<T> IngestionPipeline<T> {
    /// Construct an ingestion pipeline with a transcode handoff implementation.
    pub const fn new(transcode: T) -> Self {
        Self { transcode }
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

/// Result of source-file ingestion and transcode handoff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngestedSourceFile {
    /// Source file accepted by ingestion.
    pub source_file: SourceFile,
    /// Transcode handoff receipt.
    pub transcode: TranscodeReceipt,
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
