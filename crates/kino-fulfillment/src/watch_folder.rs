//! First-party watch-folder fulfillment provider.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant},
};

use kino_core::{CanonicalIdentityId, Id, config::WatchFolderProviderConfig};

use crate::{
    ConfiguredFulfillmentProvider, FulfillmentProvider, FulfillmentProviderArgs,
    FulfillmentProviderCancelResult, FulfillmentProviderCapabilities,
    FulfillmentProviderCapability, FulfillmentProviderCleanup, FulfillmentProviderError,
    FulfillmentProviderFuture, FulfillmentProviderJobHandle, FulfillmentProviderJobStatus,
    FulfillmentProviderProgress, FulfillmentProviderResult,
};

/// Stable id for the first-party watch-folder provider.
pub const WATCH_FOLDER_PROVIDER_ID: &str = "watch-folder";
/// Default time a file size must remain unchanged before ingestion.
pub const WATCH_FOLDER_STABILITY_WINDOW: Duration = Duration::from_secs(5);

const WATCH_FOLDER_CAPABILITIES: &[FulfillmentProviderCapability] =
    &[FulfillmentProviderCapability::AnyMedia];

/// First-party provider for user-supplied files arriving in a watched directory.
pub struct WatchFolderProvider {
    path: PathBuf,
    preference: i32,
    stability_window: Duration,
    jobs: Mutex<HashMap<String, WatchFolderJobState>>,
}

impl WatchFolderProvider {
    /// Construct a watch-folder provider from validated startup config.
    pub fn from_config(config: &WatchFolderProviderConfig) -> Self {
        Self::new_with_stability_window(
            config.path.clone(),
            config.preference,
            Duration::from_secs(config.stability_seconds),
        )
    }

    /// Construct a watch-folder provider.
    pub fn new(path: impl Into<PathBuf>, preference: i32) -> Self {
        Self::new_with_stability_window(path, preference, WATCH_FOLDER_STABILITY_WINDOW)
    }

    /// Construct a watch-folder provider with an explicit stability window.
    pub fn new_with_stability_window(
        path: impl Into<PathBuf>,
        preference: i32,
        stability_window: Duration,
    ) -> Self {
        Self {
            path: path.into(),
            preference,
            stability_window,
            jobs: Mutex::new(HashMap::new()),
        }
    }

    /// Directory watched by this provider.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// User preference copied from provider configuration.
    pub const fn preference(&self) -> i32 {
        self.preference
    }

    /// Time a file size must remain unchanged before ingestion can start.
    pub const fn stability_window(&self) -> Duration {
        self.stability_window
    }

    /// Provider descriptor used by fulfillment planning.
    pub fn configured_provider(&self) -> ConfiguredFulfillmentProvider<'_> {
        ConfiguredFulfillmentProvider::from_provider(self, self.preference)
    }
}

impl FulfillmentProvider for WatchFolderProvider {
    fn id(&self) -> &str {
        WATCH_FOLDER_PROVIDER_ID
    }

    fn capabilities(&self) -> FulfillmentProviderCapabilities<'_> {
        FulfillmentProviderCapabilities::new(WATCH_FOLDER_CAPABILITIES)
    }

    fn start<'a>(
        &'a self,
        args: FulfillmentProviderArgs,
    ) -> FulfillmentProviderFuture<'a, FulfillmentProviderJobHandle> {
        Box::pin(async move {
            let handle = FulfillmentProviderJobHandle::new(
                WATCH_FOLDER_PROVIDER_ID,
                format!("{}:{}", args.canonical_identity_id, Id::new()),
            );

            self.jobs.lock().map_err(lock_error)?.insert(
                handle.job_id.clone(),
                WatchFolderJobState::Watching {
                    canonical_identity_id: args.canonical_identity_id,
                    observation: None,
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
            let state = {
                let jobs = self.jobs.lock().map_err(lock_error)?;
                jobs.get(handle.job_id.as_str()).cloned()
            };

            match state {
                Some(WatchFolderJobState::Watching {
                    canonical_identity_id,
                    observation,
                }) => {
                    let candidate = stable_file_candidate(&self.path).await?;
                    let (next_state, status) = next_watch_status(
                        canonical_identity_id,
                        observation,
                        candidate,
                        self.stability_window,
                    );
                    self.jobs
                        .lock()
                        .map_err(lock_error)?
                        .insert(handle.job_id.clone(), next_state);
                    Ok(status)
                }
                Some(WatchFolderJobState::Completed { source_path, .. }) => {
                    Ok(FulfillmentProviderJobStatus::Completed {
                        source_paths: vec![source_path],
                    })
                }
                Some(WatchFolderJobState::Cancelled { cleanup }) => {
                    Ok(FulfillmentProviderJobStatus::Cancelled { cleanup })
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
                Some(WatchFolderJobState::Watching { .. })
                | Some(WatchFolderJobState::Completed { .. }) => {
                    let cleanup = FulfillmentProviderCleanup::NothingToCleanUp;
                    jobs.insert(
                        handle.job_id.clone(),
                        WatchFolderJobState::Cancelled { cleanup },
                    );
                    cleanup
                }
                Some(WatchFolderJobState::Cancelled { cleanup }) => *cleanup,
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
enum WatchFolderJobState {
    Watching {
        canonical_identity_id: CanonicalIdentityId,
        observation: Option<StableFileObservation>,
    },
    Completed {
        canonical_identity_id: CanonicalIdentityId,
        source_path: PathBuf,
    },
    Cancelled {
        cleanup: FulfillmentProviderCleanup,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StableFileObservation {
    path: PathBuf,
    size: u64,
    observed_at: Instant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileSnapshot {
    path: PathBuf,
    size: u64,
}

async fn stable_file_candidate(path: &Path) -> FulfillmentProviderResult<Option<FileSnapshot>> {
    let mut entries = tokio::fs::read_dir(path).await.map_err(|err| {
        FulfillmentProviderError::transient("watch_folder_read_failed", err.to_string())
    })?;
    let mut candidates = Vec::new();

    while let Some(entry) = entries.next_entry().await.map_err(|err| {
        FulfillmentProviderError::transient("watch_folder_read_failed", err.to_string())
    })? {
        let metadata = entry.metadata().await.map_err(|err| {
            FulfillmentProviderError::transient("watch_folder_metadata_failed", err.to_string())
        })?;

        if metadata.is_file() {
            let path = entry.path();
            tokio::fs::File::open(&path).await.map_err(|err| {
                FulfillmentProviderError::permanent(
                    "watch_folder_file_unreadable",
                    format!("path {} is not readable: {err}", path.display()),
                )
            })?;
            candidates.push(FileSnapshot {
                path,
                size: metadata.len(),
            });
        }
    }

    candidates.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(candidates.into_iter().next())
}

fn next_watch_status(
    canonical_identity_id: CanonicalIdentityId,
    observation: Option<StableFileObservation>,
    candidate: Option<FileSnapshot>,
    stability_window: Duration,
) -> (WatchFolderJobState, FulfillmentProviderJobStatus) {
    let Some(candidate) = candidate else {
        return (
            WatchFolderJobState::Watching {
                canonical_identity_id,
                observation: None,
            },
            FulfillmentProviderJobStatus::Queued,
        );
    };

    let now = Instant::now();
    let stable_since = match observation {
        Some(observation)
            if observation.path == candidate.path && observation.size == candidate.size =>
        {
            observation.observed_at
        }
        _ => now,
    };

    if now.duration_since(stable_since) >= stability_window {
        let source_path = candidate.path;
        return (
            WatchFolderJobState::Completed {
                canonical_identity_id,
                source_path: source_path.clone(),
            },
            FulfillmentProviderJobStatus::Completed {
                source_paths: vec![source_path],
            },
        );
    }

    (
        WatchFolderJobState::Watching {
            canonical_identity_id,
            observation: Some(StableFileObservation {
                path: candidate.path,
                size: candidate.size,
                observed_at: stable_since,
            }),
        },
        FulfillmentProviderJobStatus::Running {
            progress: waiting_for_stability_progress(),
        },
    )
}

fn waiting_for_stability_progress() -> FulfillmentProviderProgress {
    match FulfillmentProviderProgress::new(0, 1) {
        Some(progress) => progress,
        None => unreachable!("0 of 1 progress is valid"),
    }
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
    use kino_core::{TmdbId, config::WatchFolderProviderConfig};
    use std::time::Duration;

    #[test]
    fn constructs_from_config() {
        let config = WatchFolderProviderConfig {
            path: PathBuf::from("/srv/kino/incoming"),
            preference: 7,
            stability_seconds: 9,
        };

        let provider = WatchFolderProvider::from_config(&config);

        assert_eq!(provider.id(), WATCH_FOLDER_PROVIDER_ID);
        assert_eq!(provider.path(), Path::new("/srv/kino/incoming"));
        assert_eq!(provider.preference(), 7);
        assert_eq!(provider.stability_window(), Duration::from_secs(9));
        assert_eq!(
            provider.capabilities().as_slice(),
            WATCH_FOLDER_CAPABILITIES
        );
    }

    #[test]
    fn descriptor_participates_in_selection() {
        let provider = WatchFolderProvider::new("/srv/kino/incoming", 7);
        let configured = [provider.configured_provider()];
        let plan = select_fulfillment_provider(
            ProviderSelectionContext::new(movie_identity(550)),
            &configured,
        )
        .expect("watch folder provider should be selectable");

        assert_eq!(plan.selected_provider_id.as_deref(), Some("watch-folder"));
        assert_eq!(plan.ranked_providers.len(), 1);
        assert_eq!(
            plan.ranked_providers[0].matched_capability,
            FulfillmentProviderCapability::AnyMedia
        );
        assert_eq!(plan.ranked_providers[0].preference, 7);
    }

    #[tokio::test]
    async fn lifecycle_starts_reports_queued_and_cancels()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let dir = temp_dir().await?;
        let provider =
            WatchFolderProvider::new_with_stability_window(&dir, 0, Duration::from_millis(20));
        let handle = provider
            .start(FulfillmentProviderArgs::new(movie_identity(550)))
            .await?;

        assert_eq!(handle.provider_id, WATCH_FOLDER_PROVIDER_ID);
        assert_eq!(
            provider.status(&handle).await?,
            FulfillmentProviderJobStatus::Queued
        );

        let cancelled = provider.cancel(&handle).await?;

        assert_eq!(
            cancelled,
            FulfillmentProviderCancelResult::new(
                handle.clone(),
                FulfillmentProviderCleanup::NothingToCleanUp
            )
        );
        assert_eq!(
            provider.status(&handle).await?,
            FulfillmentProviderJobStatus::Cancelled {
                cleanup: FulfillmentProviderCleanup::NothingToCleanUp,
            }
        );

        tokio::fs::remove_dir(dir).await?;
        Ok(())
    }

    #[tokio::test]
    async fn complete_file_is_returned_after_stability_window()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let dir = temp_dir().await?;
        let path = dir.join("ready.mkv");
        let provider =
            WatchFolderProvider::new_with_stability_window(&dir, 0, Duration::from_millis(20));
        let handle = provider
            .start(FulfillmentProviderArgs::new(movie_identity(550)))
            .await?;
        tokio::fs::write(&path, b"complete").await?;

        assert_eq!(
            provider.status(&handle).await?,
            FulfillmentProviderJobStatus::Running {
                progress: waiting_for_stability_progress(),
            }
        );
        tokio::time::sleep(Duration::from_millis(25)).await;

        assert_eq!(
            provider.status(&handle).await?,
            FulfillmentProviderJobStatus::Completed {
                source_paths: vec![path.clone()],
            }
        );

        tokio::fs::remove_file(path).await?;
        tokio::fs::remove_dir(dir).await?;
        Ok(())
    }

    #[tokio::test]
    async fn growing_file_is_not_completed_until_size_stays_stable()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let dir = temp_dir().await?;
        let path = dir.join("growing.mkv");
        let provider =
            WatchFolderProvider::new_with_stability_window(&dir, 0, Duration::from_millis(25));
        let handle = provider
            .start(FulfillmentProviderArgs::new(movie_identity(550)))
            .await?;
        tokio::fs::write(&path, b"partial").await?;

        assert_eq!(
            provider.status(&handle).await?,
            FulfillmentProviderJobStatus::Running {
                progress: waiting_for_stability_progress(),
            }
        );
        tokio::time::sleep(Duration::from_millis(15)).await;
        tokio::fs::write(&path, b"partial-and-still-growing").await?;

        assert_eq!(
            provider.status(&handle).await?,
            FulfillmentProviderJobStatus::Running {
                progress: waiting_for_stability_progress(),
            }
        );
        tokio::time::sleep(Duration::from_millis(15)).await;
        assert_eq!(
            provider.status(&handle).await?,
            FulfillmentProviderJobStatus::Running {
                progress: waiting_for_stability_progress(),
            }
        );
        tokio::time::sleep(Duration::from_millis(15)).await;

        assert_eq!(
            provider.status(&handle).await?,
            FulfillmentProviderJobStatus::Completed {
                source_paths: vec![path.clone()],
            }
        );

        tokio::fs::remove_file(path).await?;
        tokio::fs::remove_dir(dir).await?;
        Ok(())
    }

    #[tokio::test]
    async fn unknown_job_is_permanent_error() {
        let provider = WatchFolderProvider::new("/srv/kino/incoming", 0);
        let handle = FulfillmentProviderJobHandle::new(WATCH_FOLDER_PROVIDER_ID, "missing");
        let err = provider
            .status(&handle)
            .await
            .expect_err("unknown job should fail");

        assert!(!err.is_transient());
        assert_eq!(err.code(), "unknown_job");
    }

    fn movie_identity(tmdb_id: u32) -> CanonicalIdentityId {
        match TmdbId::new(tmdb_id) {
            Some(tmdb_id) => CanonicalIdentityId::tmdb_movie(tmdb_id),
            None => panic!("test tmdb id must be positive"),
        }
    }

    async fn temp_dir() -> std::result::Result<PathBuf, Box<dyn std::error::Error>> {
        let path = std::env::temp_dir().join(format!("kino-watch-folder-{}", Id::new()));
        tokio::fs::create_dir(&path).await?;
        Ok(path)
    }
}
