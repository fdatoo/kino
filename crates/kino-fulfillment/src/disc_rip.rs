//! First-party disc-rip import provider.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Mutex,
};

use kino_core::{Id, config::DiscRipProviderConfig};

use crate::{
    ConfiguredFulfillmentProvider, FulfillmentProvider, FulfillmentProviderArgs,
    FulfillmentProviderCancelResult, FulfillmentProviderCapabilities,
    FulfillmentProviderCapability, FulfillmentProviderCleanup, FulfillmentProviderError,
    FulfillmentProviderFuture, FulfillmentProviderJobHandle, FulfillmentProviderJobStatus,
    FulfillmentProviderResult,
};

/// Stable id for the first-party disc-rip import provider.
pub const DISC_RIP_PROVIDER_ID: &str = "disc-rip";

const DISC_RIP_CAPABILITIES: &[FulfillmentProviderCapability] =
    &[FulfillmentProviderCapability::AnyMedia];

/// First-party provider for importing MakeMKV-style output directories.
pub struct DiscRipProvider {
    path: PathBuf,
    preference: i32,
    jobs: Mutex<HashMap<String, DiscRipJobState>>,
}

impl DiscRipProvider {
    /// Construct a disc-rip provider from validated startup config.
    pub fn from_config(config: &DiscRipProviderConfig) -> Self {
        Self::new(config.path.clone(), config.preference)
    }

    /// Construct a disc-rip provider.
    pub fn new(path: impl Into<PathBuf>, preference: i32) -> Self {
        Self {
            path: path.into(),
            preference,
            jobs: Mutex::new(HashMap::new()),
        }
    }

    /// Directory scanned by this provider.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// User preference copied from provider configuration.
    pub const fn preference(&self) -> i32 {
        self.preference
    }

    /// Provider descriptor used by fulfillment planning.
    pub fn configured_provider(&self) -> ConfiguredFulfillmentProvider<'_> {
        ConfiguredFulfillmentProvider::from_provider(self, self.preference)
    }
}

impl FulfillmentProvider for DiscRipProvider {
    fn id(&self) -> &str {
        DISC_RIP_PROVIDER_ID
    }

    fn capabilities(&self) -> FulfillmentProviderCapabilities<'_> {
        FulfillmentProviderCapabilities::new(DISC_RIP_CAPABILITIES)
    }

    fn start<'a>(
        &'a self,
        args: FulfillmentProviderArgs,
    ) -> FulfillmentProviderFuture<'a, FulfillmentProviderJobHandle> {
        Box::pin(async move {
            let source_paths = discover_disc_rip_files(&self.path).await?;
            if source_paths.is_empty() {
                return Err(FulfillmentProviderError::permanent(
                    "disc_rip_empty",
                    format!(
                        "directory {} contains no supported media files",
                        self.path.display()
                    ),
                ));
            }

            let handle = FulfillmentProviderJobHandle::new(
                DISC_RIP_PROVIDER_ID,
                format!("{}:{}", args.canonical_identity_id, Id::new()),
            );

            self.jobs.lock().map_err(lock_error)?.insert(
                handle.job_id.clone(),
                DiscRipJobState::Completed { source_paths },
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
                Some(DiscRipJobState::Completed { source_paths }) => {
                    Ok(FulfillmentProviderJobStatus::Completed {
                        source_paths: source_paths.clone(),
                    })
                }
                Some(DiscRipJobState::Cancelled { cleanup }) => {
                    Ok(FulfillmentProviderJobStatus::Cancelled { cleanup: *cleanup })
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
            let cleanup = match jobs.get_mut(handle.job_id.as_str()) {
                Some(DiscRipJobState::Completed { .. }) => {
                    let cleanup = FulfillmentProviderCleanup::NothingToCleanUp;
                    jobs.insert(
                        handle.job_id.clone(),
                        DiscRipJobState::Cancelled { cleanup },
                    );
                    cleanup
                }
                Some(DiscRipJobState::Cancelled { cleanup }) => *cleanup,
                None => return Err(unknown_job()),
            };

            Ok(FulfillmentProviderCancelResult::new(
                handle.clone(),
                cleanup,
            ))
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DiscRipJobState {
    Completed { source_paths: Vec<PathBuf> },
    Cancelled { cleanup: FulfillmentProviderCleanup },
}

async fn discover_disc_rip_files(path: &Path) -> FulfillmentProviderResult<Vec<PathBuf>> {
    let mut directories = vec![path.to_path_buf()];
    let mut source_paths = Vec::new();

    while let Some(directory) = directories.pop() {
        let mut entries = tokio::fs::read_dir(&directory).await.map_err(|err| {
            FulfillmentProviderError::transient("disc_rip_read_failed", err.to_string())
        })?;

        while let Some(entry) = entries.next_entry().await.map_err(|err| {
            FulfillmentProviderError::transient("disc_rip_read_failed", err.to_string())
        })? {
            let metadata = entry.metadata().await.map_err(|err| {
                FulfillmentProviderError::transient("disc_rip_metadata_failed", err.to_string())
            })?;
            let path = entry.path();

            if metadata.is_dir() {
                directories.push(path);
            } else if metadata.is_file() && is_supported_disc_rip_file(&path) {
                validate_readable_file(&path).await?;
                source_paths.push(path);
            }
        }
    }

    source_paths.sort_by(|left, right| {
        left.file_name()
            .cmp(&right.file_name())
            .then_with(|| left.cmp(right))
    });
    Ok(source_paths)
}

fn is_supported_disc_rip_file(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "mkv" | "m4v" | "mp4" | "m2ts" | "ts"
            )
        })
}

async fn validate_readable_file(path: &Path) -> FulfillmentProviderResult<()> {
    tokio::fs::File::open(path).await.map_err(|err| {
        FulfillmentProviderError::permanent(
            "disc_rip_file_unreadable",
            format!("path {} is not readable: {err}", path.display()),
        )
    })?;
    Ok(())
}

fn lock_error<T>(err: std::sync::PoisonError<T>) -> FulfillmentProviderError {
    FulfillmentProviderError::transient("lock_failed", err.to_string())
}

fn unknown_job() -> FulfillmentProviderError {
    FulfillmentProviderError::permanent("unknown_job", "job handle is not active")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ProviderSelectionContext, select_fulfillment_provider};
    use kino_core::{CanonicalIdentityId, TmdbId};

    #[test]
    fn constructs_from_config() {
        let config = DiscRipProviderConfig {
            path: PathBuf::from("/srv/kino/rips"),
            preference: 4,
        };

        let provider = DiscRipProvider::from_config(&config);

        assert_eq!(provider.id(), DISC_RIP_PROVIDER_ID);
        assert_eq!(provider.path(), Path::new("/srv/kino/rips"));
        assert_eq!(provider.preference(), 4);
        assert_eq!(provider.capabilities().as_slice(), DISC_RIP_CAPABILITIES);
    }

    #[test]
    fn descriptor_participates_in_selection() {
        let provider = DiscRipProvider::new("/srv/kino/rips", 4);
        let configured = [provider.configured_provider()];
        let plan = select_fulfillment_provider(
            ProviderSelectionContext::new(movie_identity(550)),
            &configured,
        )
        .expect("disc rip provider should be selectable");

        assert_eq!(plan.selected_provider_id.as_deref(), Some("disc-rip"));
        assert_eq!(plan.ranked_providers.len(), 1);
        assert_eq!(
            plan.ranked_providers[0].matched_capability,
            FulfillmentProviderCapability::AnyMedia
        );
        assert_eq!(plan.ranked_providers[0].preference, 4);
    }

    #[tokio::test]
    async fn single_makemkv_file_completes_with_source_path()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let dir = temp_dir().await?;
        let file = dir.join("title_t00.mkv");
        tokio::fs::write(&file, b"movie").await?;
        let provider = DiscRipProvider::new(&dir, 0);

        let handle = provider
            .start(FulfillmentProviderArgs::new(movie_identity(550)))
            .await?;

        assert_eq!(handle.provider_id, DISC_RIP_PROVIDER_ID);
        assert_eq!(
            provider.status(&handle).await?,
            FulfillmentProviderJobStatus::Completed {
                source_paths: vec![file.clone()],
            }
        );

        tokio::fs::remove_file(file).await?;
        tokio::fs::remove_dir(dir).await?;
        Ok(())
    }

    #[tokio::test]
    async fn episode_pack_returns_all_media_files_in_order()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let dir = temp_dir().await?;
        let extras = dir.join("extras");
        tokio::fs::create_dir(&extras).await?;
        let first = dir.join("title_t00.mkv");
        let second = dir.join("title_t01.mkv");
        let nested = extras.join("title_t02.m4v");
        tokio::fs::write(&second, b"episode 2").await?;
        tokio::fs::write(&first, b"episode 1").await?;
        tokio::fs::write(&nested, b"episode 3").await?;
        tokio::fs::write(dir.join("discatt.dat"), b"ignored").await?;
        let provider = DiscRipProvider::new(&dir, 0);

        let handle = provider
            .start(FulfillmentProviderArgs::new(movie_identity(550)))
            .await?;

        assert_eq!(
            provider.status(&handle).await?,
            FulfillmentProviderJobStatus::Completed {
                source_paths: vec![first.clone(), second.clone(), nested.clone()],
            }
        );

        tokio::fs::remove_file(first).await?;
        tokio::fs::remove_file(second).await?;
        tokio::fs::remove_file(nested).await?;
        tokio::fs::remove_file(dir.join("discatt.dat")).await?;
        tokio::fs::remove_dir(extras).await?;
        tokio::fs::remove_dir(dir).await?;
        Ok(())
    }

    #[tokio::test]
    async fn empty_directory_is_permanent_error()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let dir = temp_dir().await?;
        let provider = DiscRipProvider::new(&dir, 0);

        let err = provider
            .start(FulfillmentProviderArgs::new(movie_identity(550)))
            .await
            .expect_err("empty disc-rip directory should fail");

        assert_eq!(err.code(), "disc_rip_empty");
        assert!(!err.is_transient());
        tokio::fs::remove_dir(dir).await?;
        Ok(())
    }

    #[tokio::test]
    async fn unknown_job_is_permanent_error() {
        let provider = DiscRipProvider::new("/srv/kino/rips", 0);
        let handle = FulfillmentProviderJobHandle::new(DISC_RIP_PROVIDER_ID, "missing");
        let err = provider
            .status(&handle)
            .await
            .expect_err("unknown job should fail");

        assert!(!err.is_transient());
        assert_eq!(err.code(), "unknown_job");
    }

    async fn temp_dir() -> std::result::Result<PathBuf, Box<dyn std::error::Error>> {
        let path = std::env::temp_dir().join(format!("kino-disc-rip-{}", Id::new()));
        tokio::fs::create_dir(&path).await?;
        Ok(path)
    }

    fn movie_identity(tmdb_id: u32) -> CanonicalIdentityId {
        match TmdbId::new(tmdb_id) {
            Some(tmdb_id) => CanonicalIdentityId::tmdb_movie(tmdb_id),
            None => panic!("test tmdb id must be positive"),
        }
    }
}
