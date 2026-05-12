//! Autonomous filesystem watcher that drives drops into the ingestion pipeline.
//!
//! The first-party [`WatchFolderProvider`](kino_fulfillment::WatchFolderProvider)
//! is a request-driven fulfillment alternative: an operator already has a
//! resolved request and the provider waits for a matching file. This module is
//! the inverse side of the same coin: a file appears in the configured incoming
//! directory and the intake creates a request for it, resolves it against TMDB,
//! and feeds it through the same ingestion pipeline the HTTP `manual-import`
//! endpoint uses. Without this loop the binary has no autonomous trigger for
//! ingest.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use kino_core::{
    CanonicalIdentityId, CanonicalLayoutTransfer, Id, TmdbId, config::WatchFolderProviderConfig,
};
use kino_db::Db;
use kino_fulfillment::{
    FulfillmentProvider, FulfillmentProviderArgs, ManualImportProvider, NewRequest,
    RequestEventActor, RequestIdentityProvenance, RequestMatchCandidateInput, RequestRequester,
    RequestService, RequestState, RequestTransition,
    movie::parse_movie_request,
    rank_match_candidates,
    tmdb::{self, TmdbClient},
    tv::parse_tv_request,
};
use kino_library::{
    CanonicalLayoutWriter, CatalogService, LibraryScanService, SubtitleReocrService,
};
use kino_transcode::TranscodeHandOff;
use tokio::{task::JoinHandle, time::Instant};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::{ingestion_orchestrator::ingest_request, request::AppState};

const MEDIA_EXTENSIONS: &[&str] = &[
    "mkv", "mp4", "mov", "m4v", "avi", "webm", "mpg", "mpeg", "ts", "m2ts", "wmv",
];

const MIN_POLL_INTERVAL: Duration = Duration::from_secs(2);
const PLAN_SUMMARY: &str = "watch folder intake";

/// Dependencies needed to drive ingestion from a filesystem watcher.
pub struct WatchFolderIntakeDeps {
    /// Shared database handle.
    pub db: Db,
    /// Library root where canonical files land.
    pub library_root: PathBuf,
    /// Artwork cache directory used by metadata enrichment.
    pub artwork_cache_dir: PathBuf,
    /// Canonical layout transfer mode (move or hard-link).
    pub canonical_transfer: CanonicalLayoutTransfer,
    /// Subtitle re-OCR service consumed by the ingest pipeline.
    pub subtitle_reocr: SubtitleReocrService,
    /// Configured TMDB client; required for autonomous identity resolution.
    pub tmdb_client: Option<TmdbClient>,
    /// Shared transcode hand-off used by the ingest pipeline.
    pub transcode: Arc<dyn TranscodeHandOff>,
}

/// Handle for a spawned intake task.
pub struct WatchFolderIntake {
    shutdown: CancellationToken,
    task: JoinHandle<()>,
}

impl WatchFolderIntake {
    /// Signal the task to exit on its next poll boundary and wait for it.
    pub async fn shutdown(self) {
        self.shutdown.cancel();
        let _ = self.task.await;
    }
}

/// Spawn the autonomous watch-folder intake loop.
pub fn spawn(deps: WatchFolderIntakeDeps, config: WatchFolderProviderConfig) -> WatchFolderIntake {
    let shutdown = CancellationToken::new();
    let task_shutdown = shutdown.clone();
    let task = tokio::spawn(async move { run(deps, config, task_shutdown).await });

    WatchFolderIntake { shutdown, task }
}

async fn run(
    deps: WatchFolderIntakeDeps,
    config: WatchFolderProviderConfig,
    shutdown: CancellationToken,
) {
    let stability_window = Duration::from_secs(config.stability_seconds);
    let poll_interval = poll_interval(stability_window);
    let state = AppState {
        db: deps.db.clone(),
        requests: RequestService::new(deps.db.clone()),
        manual_imports: Arc::new(ManualImportProvider::new()),
        catalog: CatalogService::new(deps.db.clone()),
        library_scans: LibraryScanService::new(deps.db.clone(), deps.library_root.clone()),
        subtitle_reocr: deps.subtitle_reocr,
        tmdb: deps.tmdb_client,
        canonical_layout: CanonicalLayoutWriter::new(&deps.library_root, deps.canonical_transfer),
        transcode: deps.transcode,
        library_root: deps.library_root.clone(),
        artwork_cache_dir: deps.artwork_cache_dir,
    };

    info!(
        path = %config.path.display(),
        stability_seconds = config.stability_seconds,
        poll_interval_ms = poll_interval.as_millis() as u64,
        "watch folder intake started",
    );

    if state.tmdb.is_none() {
        warn!(
            "watch folder intake has no TMDB client; files will be skipped until KINO_TMDB__API_KEY is configured"
        );
    }

    let mut observations: HashMap<PathBuf, FileObservation> = HashMap::new();
    let mut attempted: HashSet<PathBuf> = HashSet::new();

    loop {
        tokio::select! {
            () = shutdown.cancelled() => {
                info!("watch folder intake stopping");
                return;
            }
            () = tokio::time::sleep(poll_interval) => {}
        }

        if let Err(error) = tick(
            &state,
            &config.path,
            stability_window,
            &mut observations,
            &mut attempted,
        )
        .await
        {
            warn!(error = %error, "watch folder intake tick failed");
        }
    }
}

async fn tick(
    state: &AppState,
    directory: &Path,
    stability_window: Duration,
    observations: &mut HashMap<PathBuf, FileObservation>,
    attempted: &mut HashSet<PathBuf>,
) -> std::io::Result<()> {
    let snapshots = scan_directory(directory).await?;
    let present: HashSet<PathBuf> = snapshots.iter().map(|snap| snap.path.clone()).collect();

    observations.retain(|path, _| present.contains(path));
    attempted.retain(|path| present.contains(path));

    let now = Instant::now();
    let mut ready: Option<PathBuf> = None;

    for snapshot in snapshots {
        if attempted.contains(&snapshot.path) {
            continue;
        }
        let entry = observations
            .entry(snapshot.path.clone())
            .or_insert_with(|| FileObservation::new(snapshot.size, now));
        if entry.update(snapshot.size, now, stability_window) && ready.is_none() {
            ready = Some(snapshot.path.clone());
        }
    }

    let Some(path) = ready else {
        return Ok(());
    };

    attempted.insert(path.clone());
    let path_display = path.display().to_string();
    info!(path = %path_display, "watch folder stable file detected");

    match ingest_stable_file(state, path).await {
        Ok(()) => {
            info!(path = %path_display, "watch folder ingest accepted");
        }
        Err(error) => {
            error!(path = %path_display, error = %error, "watch folder ingest failed");
        }
    }

    Ok(())
}

async fn ingest_stable_file(state: &AppState, path: PathBuf) -> Result<(), IntakeError> {
    let Some(tmdb) = state.tmdb.as_ref() else {
        return Err(IntakeError::TmdbUnavailable);
    };
    let raw_query =
        filename_query(&path).ok_or(IntakeError::UnsupportedFilename { path: path.clone() })?;
    let candidates = fetch_tmdb_candidates(tmdb, &raw_query).await?;
    if candidates.is_empty() {
        return Err(IntakeError::NoCandidates { query: raw_query });
    }
    let top_identity = top_candidate_identity(&raw_query, &candidates)?;

    let request = state
        .requests
        .create(NewRequest {
            target_raw_query: raw_query.as_str(),
            requester: RequestRequester::Anonymous,
            actor: Some(RequestEventActor::System),
            message: Some("watch folder intake"),
        })
        .await?;
    let request_id = request.request.id;

    let resolved = state
        .requests
        .resolve_matches(
            request_id,
            candidates,
            Some(RequestEventActor::System),
            Some("resolved candidates from TMDB"),
        )
        .await?;

    let mut current = resolved;
    if current.request.state != RequestState::Resolved
        && current.request.state != RequestState::Satisfied
    {
        current = state
            .requests
            .resolve_identity(
                request_id,
                top_identity,
                RequestIdentityProvenance::MatchScoring,
                Some(RequestEventActor::System),
                Some("watch folder auto-resolved top candidate"),
            )
            .await?;
    }

    if current.request.state == RequestState::Satisfied {
        info!(
            request_id = %request_id,
            path = %path.display(),
            "watch folder candidate is already satisfied; removing duplicate from intake",
        );
        if let Err(err) = tokio::fs::remove_file(&path).await
            && err.kind() != std::io::ErrorKind::NotFound
        {
            warn!(
                path = %path.display(),
                error = %err,
                "watch folder failed to remove duplicate source",
            );
        }
        return Ok(());
    }

    state
        .requests
        .prepare_for_manual_import(request_id, PLAN_SUMMARY)
        .await?;

    let canonical_identity_id = state
        .requests
        .get(request_id)
        .await?
        .request
        .target
        .canonical_identity_id
        .ok_or(IntakeError::MissingCanonicalIdentity { request_id })?;

    let handle = state
        .manual_imports
        .start(FulfillmentProviderArgs::new(canonical_identity_id).with_source_path(&path))
        .await
        .map_err(IntakeError::ManualImport)?;

    let message = format!(
        "watch folder accepted {} as {}",
        path.display(),
        handle.job_id
    );
    state
        .requests
        .transition(
            request_id,
            RequestTransition::StartIngesting,
            Some(RequestEventActor::System),
            Some(message.as_str()),
        )
        .await?;

    let detail = ingest_request(state, request_id, path.clone(), handle.job_id)
        .await
        .map_err(|err| IntakeError::Ingest(err.to_string()))?;
    if detail.request.state != RequestState::Satisfied {
        return Err(IntakeError::Ingest(format!(
            "request ended in state {state:?}",
            state = detail.request.state
        )));
    }

    match tokio::fs::remove_file(&path).await {
        Ok(()) => {
            debug!(path = %path.display(), "watch folder removed ingested source");
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            warn!(
                path = %path.display(),
                error = %err,
                "watch folder failed to remove ingested source",
            );
        }
    }

    Ok(())
}

async fn fetch_tmdb_candidates(
    tmdb: &TmdbClient,
    raw_query: &str,
) -> Result<Vec<RequestMatchCandidateInput>, IntakeError> {
    let mut candidates = Vec::new();

    if let Ok(movie_request) = parse_movie_request(raw_query) {
        let movies = tmdb
            .search_movies(&movie_request.title, movie_request.release_year)
            .await?;
        for movie in movies {
            let tmdb_id =
                TmdbId::new(movie.movie_id.get()).ok_or(IntakeError::InvalidTmdbCandidate {
                    value: movie.movie_id.get(),
                })?;
            candidates.push(RequestMatchCandidateInput {
                canonical_identity_id: CanonicalIdentityId::tmdb_movie(tmdb_id),
                title: movie.title,
                year: movie.release_year,
                popularity: movie.popularity,
            });
        }
    }

    if let Ok(tv_request) = parse_tv_request(raw_query) {
        let series = tmdb
            .search_tv(&tv_request.title, tv_request.first_air_year)
            .await?;
        for series in series {
            let tmdb_id =
                TmdbId::new(series.series_id.get()).ok_or(IntakeError::InvalidTmdbCandidate {
                    value: series.series_id.get(),
                })?;
            candidates.push(RequestMatchCandidateInput {
                canonical_identity_id: CanonicalIdentityId::tmdb_tv_series(tmdb_id),
                title: series.name,
                year: series.first_air_year,
                popularity: series.popularity,
            });
        }
    }

    Ok(candidates)
}

fn top_candidate_identity(
    raw_query: &str,
    candidates: &[RequestMatchCandidateInput],
) -> Result<CanonicalIdentityId, IntakeError> {
    let ranked = rank_match_candidates(raw_query, candidates.to_vec())
        .map_err(|err| IntakeError::Ranking(err.to_string()))?;
    ranked
        .into_iter()
        .next()
        .map(|candidate| candidate.canonical_identity_id)
        .ok_or(IntakeError::NoCandidates {
            query: raw_query.to_owned(),
        })
}

fn poll_interval(stability_window: Duration) -> Duration {
    let candidate = stability_window / 2;
    if candidate < MIN_POLL_INTERVAL {
        MIN_POLL_INTERVAL
    } else {
        candidate
    }
}

fn filename_query(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?.trim();
    if stem.is_empty() {
        return None;
    }
    Some(stem.to_owned())
}

async fn scan_directory(directory: &Path) -> std::io::Result<Vec<FileSnapshot>> {
    let mut entries = tokio::fs::read_dir(directory).await?;
    let mut snapshots = Vec::new();
    while let Some(entry) = entries.next_entry().await? {
        let metadata = entry.metadata().await?;
        if !metadata.is_file() {
            continue;
        }
        let path = entry.path();
        if !is_media_file(&path) {
            debug!(path = %path.display(), "watch folder intake skipping non-media file");
            continue;
        }
        snapshots.push(FileSnapshot {
            path,
            size: metadata.len(),
        });
    }
    snapshots.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(snapshots)
}

fn is_media_file(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    if name.starts_with('.') {
        return false;
    }
    let Some(extension) = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
    else {
        return false;
    };
    MEDIA_EXTENSIONS.iter().any(|allowed| *allowed == extension)
}

#[derive(Debug, Clone, Copy)]
struct FileObservation {
    size: u64,
    observed_at: Instant,
}

impl FileObservation {
    fn new(size: u64, observed_at: Instant) -> Self {
        Self { size, observed_at }
    }

    /// Update the observation with the current size and return `true` if it has
    /// been stable for the configured window.
    fn update(&mut self, size: u64, now: Instant, window: Duration) -> bool {
        if size != self.size {
            self.size = size;
            self.observed_at = now;
            return false;
        }
        now.saturating_duration_since(self.observed_at) >= window
    }
}

#[derive(Debug, Clone)]
struct FileSnapshot {
    path: PathBuf,
    size: u64,
}

#[derive(Debug, thiserror::Error)]
enum IntakeError {
    #[error("no tmdb client configured")]
    TmdbUnavailable,

    #[error("filename {path} cannot be turned into a TMDB query", path = .path.display())]
    UnsupportedFilename { path: PathBuf },

    #[error("tmdb returned no candidates for query {query}")]
    NoCandidates { query: String },

    #[error("tmdb candidate id {value} is not a valid TMDB id")]
    InvalidTmdbCandidate { value: u32 },

    #[error("ranking candidates failed: {0}")]
    Ranking(String),

    #[error("request {request_id} has no canonical identity after auto-resolve")]
    MissingCanonicalIdentity { request_id: Id },

    #[error(transparent)]
    Fulfillment(#[from] kino_fulfillment::Error),

    #[error(transparent)]
    Tmdb(#[from] tmdb::Error),

    #[error("manual import provider rejected the file: {0}")]
    ManualImport(kino_fulfillment::FulfillmentProviderError),

    #[error("ingestion pipeline failed: {0}")]
    Ingest(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tempfile::TempDir;
    use tokio::time::Instant;

    #[test]
    fn poll_interval_floors_at_two_seconds() {
        assert_eq!(poll_interval(Duration::from_secs(1)), MIN_POLL_INTERVAL);
        assert_eq!(poll_interval(Duration::from_secs(4)), MIN_POLL_INTERVAL);
    }

    #[test]
    fn poll_interval_halves_long_windows() {
        assert_eq!(
            poll_interval(Duration::from_secs(10)),
            Duration::from_secs(5)
        );
        assert_eq!(
            poll_interval(Duration::from_secs(30)),
            Duration::from_secs(15)
        );
    }

    #[test]
    fn is_media_file_accepts_known_extensions() {
        assert!(is_media_file(Path::new("/incoming/show.mkv")));
        assert!(is_media_file(Path::new("/incoming/movie.MP4")));
        assert!(is_media_file(Path::new("/incoming/spaced name.m4v")));
    }

    #[test]
    fn is_media_file_rejects_other_files() {
        assert!(!is_media_file(Path::new("/incoming/notes.txt")));
        assert!(!is_media_file(Path::new("/incoming/.hidden.mkv")));
        assert!(!is_media_file(Path::new("/incoming/no-extension")));
    }

    #[test]
    fn filename_query_uses_the_stem() {
        assert_eq!(
            filename_query(Path::new("/incoming/Test Movie (2023).mkv")).as_deref(),
            Some("Test Movie (2023)"),
        );
    }

    #[test]
    fn observation_treats_size_change_as_growth() {
        let start = Instant::now();
        let later = start + Duration::from_secs(10);
        let window = Duration::from_secs(5);

        let mut observation = FileObservation::new(100, start);
        assert!(!observation.update(200, start + Duration::from_secs(1), window));
        assert!(observation.update(200, later, window));
    }

    #[test]
    fn observation_requires_window_of_unchanged_size() {
        let start = Instant::now();
        let window = Duration::from_secs(5);

        let mut observation = FileObservation::new(100, start);
        assert!(!observation.update(100, start + Duration::from_secs(2), window));
        assert!(observation.update(100, start + Duration::from_secs(5), window));
    }

    #[tokio::test]
    async fn scan_directory_returns_sorted_media_files()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let dir = TempDir::new()?;
        tokio::fs::write(dir.path().join("notes.txt"), b"ignore").await?;
        tokio::fs::write(dir.path().join("b.mkv"), b"film").await?;
        tokio::fs::write(dir.path().join("a.mp4"), b"film").await?;
        tokio::fs::write(dir.path().join(".hidden.mkv"), b"film").await?;

        let snapshots = scan_directory(dir.path()).await?;
        let names: Vec<_> = snapshots
            .iter()
            .map(|snapshot| {
                snapshot
                    .path
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(names, vec!["a.mp4", "b.mkv"]);
        Ok(())
    }
}
