//! First-party watch-folder fulfillment provider.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Mutex,
};

use kino_core::{CanonicalIdentityId, Id, config::WatchFolderProviderConfig};

use crate::{
    ConfiguredFulfillmentProvider, FulfillmentProvider, FulfillmentProviderArgs,
    FulfillmentProviderCancelResult, FulfillmentProviderCapabilities,
    FulfillmentProviderCapability, FulfillmentProviderCleanup, FulfillmentProviderError,
    FulfillmentProviderFuture, FulfillmentProviderJobHandle, FulfillmentProviderJobStatus,
};

/// Stable id for the first-party watch-folder provider.
pub const WATCH_FOLDER_PROVIDER_ID: &str = "watch-folder";

const WATCH_FOLDER_CAPABILITIES: &[FulfillmentProviderCapability] =
    &[FulfillmentProviderCapability::AnyMedia];

/// First-party provider for user-supplied files arriving in a watched directory.
pub struct WatchFolderProvider {
    path: PathBuf,
    preference: i32,
    jobs: Mutex<HashMap<String, WatchFolderJobState>>,
}

impl WatchFolderProvider {
    /// Construct a watch-folder provider from validated startup config.
    pub fn from_config(config: &WatchFolderProviderConfig) -> Self {
        Self::new(config.path.clone(), config.preference)
    }

    /// Construct a watch-folder provider.
    pub fn new(path: impl Into<PathBuf>, preference: i32) -> Self {
        Self {
            path: path.into(),
            preference,
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
                WatchFolderJobState::Queued {
                    canonical_identity_id: args.canonical_identity_id,
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
            match jobs.get(handle.job_id.as_str()).copied() {
                Some(WatchFolderJobState::Queued { .. }) => {
                    Ok(FulfillmentProviderJobStatus::Queued)
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
                Some(WatchFolderJobState::Queued { .. }) => {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WatchFolderJobState {
    Queued {
        canonical_identity_id: CanonicalIdentityId,
    },
    Cancelled {
        cleanup: FulfillmentProviderCleanup,
    },
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

    #[test]
    fn constructs_from_config() {
        let config = WatchFolderProviderConfig {
            path: PathBuf::from("/srv/kino/incoming"),
            preference: 7,
        };

        let provider = WatchFolderProvider::from_config(&config);

        assert_eq!(provider.id(), WATCH_FOLDER_PROVIDER_ID);
        assert_eq!(provider.path(), Path::new("/srv/kino/incoming"));
        assert_eq!(provider.preference(), 7);
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
        let provider = WatchFolderProvider::new("/srv/kino/incoming", 0);
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
}
