//! First-party manual import fulfillment provider.

use std::{
    collections::HashMap,
    io::ErrorKind,
    path::{Path, PathBuf},
    sync::Mutex,
};

use kino_core::{CanonicalIdentityId, Id};

use crate::{
    FulfillmentProvider, FulfillmentProviderArgs, FulfillmentProviderCancelResult,
    FulfillmentProviderCapabilities, FulfillmentProviderCapability, FulfillmentProviderCleanup,
    FulfillmentProviderError, FulfillmentProviderFuture, FulfillmentProviderJobHandle,
    FulfillmentProviderJobStatus,
};

/// Stable id for the first-party manual import provider.
pub const MANUAL_IMPORT_PROVIDER_ID: &str = "manual-import";

const MANUAL_IMPORT_CAPABILITIES: &[FulfillmentProviderCapability] =
    &[FulfillmentProviderCapability::AnyMedia];

/// First-party provider for admin-selected source files.
pub struct ManualImportProvider {
    jobs: Mutex<HashMap<String, ManualImportJobState>>,
}

impl ManualImportProvider {
    /// Construct a manual import provider.
    pub fn new() -> Self {
        Self {
            jobs: Mutex::new(HashMap::new()),
        }
    }
}

impl Default for ManualImportProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl FulfillmentProvider for ManualImportProvider {
    fn id(&self) -> &str {
        MANUAL_IMPORT_PROVIDER_ID
    }

    fn capabilities(&self) -> FulfillmentProviderCapabilities<'_> {
        FulfillmentProviderCapabilities::new(MANUAL_IMPORT_CAPABILITIES)
    }

    fn start<'a>(
        &'a self,
        args: FulfillmentProviderArgs,
    ) -> FulfillmentProviderFuture<'a, FulfillmentProviderJobHandle> {
        Box::pin(async move {
            let source_path = args.source_path.ok_or_else(|| {
                permanent(
                    "source_path_required",
                    "manual import requires a source path",
                )
            })?;
            validate_source_path(&source_path).await?;

            let handle = FulfillmentProviderJobHandle::new(
                MANUAL_IMPORT_PROVIDER_ID,
                format!("{}:{}", args.canonical_identity_id, Id::new()),
            );

            self.jobs.lock().map_err(lock_error)?.insert(
                handle.job_id.clone(),
                ManualImportJobState {
                    canonical_identity_id: args.canonical_identity_id,
                    source_path,
                    cancelled: false,
                },
            );

            Ok(handle)
        })
    }

    fn status<'a>(
        &'a self,
        handle: &'a FulfillmentProviderJobHandle,
    ) -> FulfillmentProviderFuture<'a, FulfillmentProviderJobStatus> {
        Box::pin(async move {
            let jobs = self.jobs.lock().map_err(lock_error)?;
            match jobs.get(handle.job_id.as_str()) {
                Some(job) if job.cancelled => Ok(FulfillmentProviderJobStatus::Cancelled {
                    cleanup: FulfillmentProviderCleanup::NothingToCleanUp,
                }),
                Some(job) => {
                    let _canonical_identity_id = job.canonical_identity_id;
                    let _source_path = job.source_path.as_path();
                    Ok(FulfillmentProviderJobStatus::Completed)
                }
                None => Err(unknown_job()),
            }
        })
    }

    fn cancel<'a>(
        &'a self,
        handle: &'a FulfillmentProviderJobHandle,
    ) -> FulfillmentProviderFuture<'a, FulfillmentProviderCancelResult> {
        Box::pin(async move {
            let mut jobs = self.jobs.lock().map_err(lock_error)?;
            let job = jobs
                .get_mut(handle.job_id.as_str())
                .ok_or_else(unknown_job)?;
            job.cancelled = true;

            Ok(FulfillmentProviderCancelResult::new(
                handle.clone(),
                FulfillmentProviderCleanup::NothingToCleanUp,
            ))
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ManualImportJobState {
    canonical_identity_id: CanonicalIdentityId,
    source_path: PathBuf,
    cancelled: bool,
}

async fn validate_source_path(path: &Path) -> Result<(), FulfillmentProviderError> {
    let metadata = tokio::fs::metadata(path)
        .await
        .map_err(|source| path_metadata_error(path, source))?;

    if !metadata.is_file() {
        return Err(permanent(
            "path_not_file",
            format!("path {} is not a file", path.display()),
        ));
    }

    tokio::fs::File::open(path)
        .await
        .map_err(|source| path_open_error(path, source))?;

    Ok(())
}

fn path_metadata_error(path: &Path, source: std::io::Error) -> FulfillmentProviderError {
    match source.kind() {
        ErrorKind::NotFound => permanent(
            "path_not_found",
            format!("path {} does not exist", path.display()),
        ),
        ErrorKind::PermissionDenied => permanent(
            "path_unreadable",
            format!("path {} is not readable: {source}", path.display()),
        ),
        _ => FulfillmentProviderError::transient(
            "path_metadata_failed",
            format!("could not read metadata for {}: {source}", path.display()),
        ),
    }
}

fn path_open_error(path: &Path, source: std::io::Error) -> FulfillmentProviderError {
    match source.kind() {
        ErrorKind::NotFound => permanent(
            "path_not_found",
            format!("path {} does not exist", path.display()),
        ),
        ErrorKind::PermissionDenied => permanent(
            "path_unreadable",
            format!("path {} is not readable: {source}", path.display()),
        ),
        _ => permanent(
            "path_unreadable",
            format!("path {} could not be opened: {source}", path.display()),
        ),
    }
}

fn permanent(code: impl Into<String>, message: impl Into<String>) -> FulfillmentProviderError {
    FulfillmentProviderError::permanent(code, message)
}

fn lock_error<T>(err: std::sync::PoisonError<T>) -> FulfillmentProviderError {
    FulfillmentProviderError::transient("lock_failed", err.to_string())
}

fn unknown_job() -> FulfillmentProviderError {
    permanent("unknown_job", "job handle is not active")
}

#[cfg(test)]
mod tests {
    use super::*;
    use kino_core::TmdbId;

    #[tokio::test]
    async fn starts_completed_job_for_readable_file()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let path = temp_path("readable.mkv");
        tokio::fs::write(&path, b"movie").await?;
        let provider = ManualImportProvider::new();

        let handle = provider
            .start(FulfillmentProviderArgs::new(movie_identity(550)).with_source_path(&path))
            .await?;

        assert_eq!(handle.provider_id, MANUAL_IMPORT_PROVIDER_ID);
        assert_eq!(
            provider.status(&handle).await?,
            FulfillmentProviderJobStatus::Completed
        );

        tokio::fs::remove_file(path).await?;
        Ok(())
    }

    #[tokio::test]
    async fn rejects_missing_source_path() {
        let provider = ManualImportProvider::new();
        let err = provider
            .start(FulfillmentProviderArgs::new(movie_identity(550)))
            .await
            .expect_err("manual import requires source path");

        assert_eq!(err.code(), "source_path_required");
        assert!(!err.is_transient());
    }

    #[tokio::test]
    async fn rejects_missing_file() {
        let provider = ManualImportProvider::new();
        let err = provider
            .start(
                FulfillmentProviderArgs::new(movie_identity(550))
                    .with_source_path(temp_path("missing.mkv")),
            )
            .await
            .expect_err("missing file should be rejected");

        assert_eq!(err.code(), "path_not_found");
        assert!(!err.is_transient());
    }

    #[tokio::test]
    async fn rejects_directory_path() -> std::result::Result<(), Box<dyn std::error::Error>> {
        let path = std::env::temp_dir().join(format!("kino-manual-import-dir-{}", Id::new()));
        tokio::fs::create_dir(&path).await?;
        let provider = ManualImportProvider::new();

        let err = provider
            .start(FulfillmentProviderArgs::new(movie_identity(550)).with_source_path(&path))
            .await
            .expect_err("directory path should be rejected");

        assert_eq!(err.code(), "path_not_file");
        tokio::fs::remove_dir(path).await?;
        Ok(())
    }

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("kino-manual-import-{}-{name}", Id::new()))
    }

    fn movie_identity(tmdb_id: u32) -> CanonicalIdentityId {
        match TmdbId::new(tmdb_id) {
            Some(tmdb_id) => CanonicalIdentityId::tmdb_movie(tmdb_id),
            None => panic!("test tmdb id must be positive"),
        }
    }
}
