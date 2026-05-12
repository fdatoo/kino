//! In-memory registry for live encodes currently producing HLS segments.

use std::{
    ops::Deref,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use dashmap::{DashMap, mapref::entry::Entry};
use kino_core::Id;
use tokio::{sync::oneshot, sync::watch, task::JoinHandle, time::sleep};
use tracing::{error, warn};

use super::store::{EphemeralStore, NewEphemeralOutput};
use crate::{FfmpegEncodeCommand, PipelineRunner, Result, verify_outputs};

const SEGMENT_POLL_INTERVAL: Duration = Duration::from_millis(100);
const SEGMENT_WAIT_TIMEOUT: Duration = Duration::from_secs(60);
const NO_SEGMENT_READY: u64 = u64::MAX;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ActiveKey {
    source_file_id: Id,
    profile_hash: [u8; 32],
}

/// Live encode registration returned while an FFmpeg process is active.
pub struct ActiveEncode {
    /// Active encode id.
    pub id: Id,
    /// Source file this active encode transcodes.
    pub source_file_id: Id,
    /// SHA-256 digest of the canonical transcode profile JSON.
    pub profile_hash: [u8; 32],
    /// Canonical transcode profile JSON.
    pub profile_json: String,
    /// Directory receiving HLS output.
    pub output_dir: PathBuf,
    /// Watch channel publishing the highest observed segment number.
    pub segment_watch: watch::Sender<u64>,
    /// Number of current request handlers sharing this active encode.
    pub refcount: AtomicUsize,
}

/// New live encode request passed to `ActiveEncodes::get_or_spawn`.
pub struct ActiveEncodeRequest {
    /// Source file this active encode transcodes.
    pub source_file_id: Id,
    /// SHA-256 digest of the canonical transcode profile JSON.
    pub profile_hash: [u8; 32],
    /// Canonical transcode profile JSON.
    pub profile_json: String,
    /// Directory receiving HLS output.
    pub output_dir: PathBuf,
    /// FFmpeg command that writes the HLS output.
    pub command: FfmpegEncodeCommand,
}

/// Reference-counted active encode lease.
pub struct ActiveEncodeLease {
    active: Arc<ActiveEncode>,
}

impl ActiveEncodeLease {
    fn new(active: Arc<ActiveEncode>) -> Self {
        Self { active }
    }
}

impl Deref for ActiveEncodeLease {
    type Target = ActiveEncode;

    fn deref(&self) -> &Self::Target {
        &self.active
    }
}

impl Drop for ActiveEncodeLease {
    fn drop(&mut self) {
        self.active.refcount.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Process-local registry that deduplicates live encodes by source/profile key.
#[derive(Clone)]
pub struct ActiveEncodes {
    store: EphemeralStore,
    runner: Arc<PipelineRunner>,
    by_key: Arc<DashMap<ActiveKey, Arc<ActiveEncode>>>,
    by_id: Arc<DashMap<Id, ActiveKey>>,
}

impl ActiveEncodes {
    /// Construct an active registry backed by an ephemeral store and runner.
    pub fn new(store: EphemeralStore, runner: Arc<PipelineRunner>) -> Self {
        Self {
            store,
            runner,
            by_key: Arc::new(DashMap::new()),
            by_id: Arc::new(DashMap::new()),
        }
    }

    /// Return an existing encode for the source/profile key.
    pub fn get(&self, source_file_id: Id, profile_hash: [u8; 32]) -> Option<ActiveEncodeLease> {
        let key = ActiveKey {
            source_file_id,
            profile_hash,
        };
        let active = self
            .by_key
            .get(&key)
            .map(|entry| Arc::clone(entry.value()))?;
        active.refcount.fetch_add(1, Ordering::AcqRel);
        Some(ActiveEncodeLease::new(active))
    }

    /// Return an existing encode for the key or spawn the provided command.
    pub async fn get_or_spawn(&self, request: ActiveEncodeRequest) -> Result<ActiveEncodeLease> {
        let key = ActiveKey {
            source_file_id: request.source_file_id,
            profile_hash: request.profile_hash,
        };

        match self.by_key.entry(key.clone()) {
            Entry::Occupied(entry) => {
                let active = Arc::clone(entry.get());
                active.refcount.fetch_add(1, Ordering::AcqRel);
                Ok(ActiveEncodeLease::new(active))
            }
            Entry::Vacant(entry) => {
                tokio::fs::create_dir_all(&request.output_dir).await?;
                let (segment_tx, _) = watch::channel(NO_SEGMENT_READY);
                let active = Arc::new(ActiveEncode {
                    id: Id::new(),
                    source_file_id: request.source_file_id,
                    profile_hash: request.profile_hash,
                    profile_json: request.profile_json,
                    output_dir: request.output_dir,
                    segment_watch: segment_tx,
                    refcount: AtomicUsize::new(1),
                });
                entry.insert(Arc::clone(&active));
                self.by_id.insert(active.id, key);
                self.spawn_tasks(Arc::clone(&active), request.command);
                Ok(ActiveEncodeLease::new(active))
            }
        }
    }

    /// Await segment `n` for an active encode id.
    pub async fn await_segment(&self, id: Id, n: u64) -> Result<()> {
        let Some(key) = self.by_id.get(&id).map(|entry| entry.clone()) else {
            return Err(crate::Error::EphemeralOutputNotFound { id });
        };
        let Some(active) = self.by_key.get(&key).map(|entry| Arc::clone(entry.value())) else {
            return Err(crate::Error::EphemeralOutputNotFound { id });
        };

        if segment_path(&active.output_dir, n).is_file() {
            return Ok(());
        }

        let mut rx = active.segment_watch.subscribe();
        let deadline = sleep(SEGMENT_WAIT_TIMEOUT);
        tokio::pin!(deadline);

        loop {
            let current = *rx.borrow_and_update();
            if (current != NO_SEGMENT_READY && current >= n)
                || segment_path(&active.output_dir, n).is_file()
            {
                return Ok(());
            }

            tokio::select! {
                changed = rx.changed() => {
                    if changed.is_err() {
                        return Err(crate::Error::EphemeralOutputNotFound { id });
                    }
                }
                () = sleep(SEGMENT_POLL_INTERVAL) => {}
                () = &mut deadline => {
                    return Err(crate::Error::LiveSegmentTimeout { id, segment: n });
                }
            }
        }
    }

    /// Transition a completed active encode into the durable ephemeral cache.
    pub async fn finish(&self, id: Id) -> Result<super::store::EphemeralOutput> {
        let Some((_, key)) = self.by_id.remove(&id) else {
            return Err(crate::Error::EphemeralOutputNotFound { id });
        };
        let Some((_, active)) = self.by_key.remove(&key) else {
            return Err(crate::Error::EphemeralOutputNotFound { id });
        };

        let size_bytes = directory_size(&active.output_dir).await?;
        self.store
            .insert(&NewEphemeralOutput {
                id: active.id,
                source_file_id: active.source_file_id,
                profile_hash: active.profile_hash,
                profile_json: active.profile_json.clone(),
                directory_path: active.output_dir.clone(),
                size_bytes,
            })
            .await
    }

    fn spawn_tasks(&self, active: Arc<ActiveEncode>, command: FfmpegEncodeCommand) {
        let active_for_watch = Arc::clone(&active);
        let watcher = tokio::spawn(async move {
            watch_segments(active_for_watch).await;
        });

        let registry = self.clone();
        let runner = Arc::clone(&self.runner);
        tokio::spawn(async move {
            run_active_encode(registry, runner, active, command, watcher).await;
        });
    }
}

async fn run_active_encode(
    registry: ActiveEncodes,
    runner: Arc<PipelineRunner>,
    active: Arc<ActiveEncode>,
    command: FfmpegEncodeCommand,
    watcher: JoinHandle<()>,
) {
    let (cancel_tx, cancel_rx) = oneshot::channel();
    let result = runner.run(command, cancel_rx).await;
    drop(cancel_tx);
    watcher.abort();

    match result.and_then(|_| verify_outputs(&active.output_dir)) {
        Ok(()) => {
            if let Err(err) = registry.finish(active.id).await {
                error!(id = %active.id, error = %err, "live transcode cache insert failed");
            }
        }
        Err(err) => {
            warn!(id = %active.id, error = %err, "live transcode failed");
            remove_active(&registry, &active);
        }
    }
}

fn remove_active(registry: &ActiveEncodes, active: &ActiveEncode) {
    let key = ActiveKey {
        source_file_id: active.source_file_id,
        profile_hash: active.profile_hash,
    };
    registry.by_id.remove(&active.id);
    registry.by_key.remove(&key);
}

async fn watch_segments(active: Arc<ActiveEncode>) {
    let mut highest_sent = 0;
    loop {
        match highest_segment(&active.output_dir).await {
            Ok(Some(highest)) if highest > highest_sent => {
                highest_sent = highest;
                if active.segment_watch.send(highest).is_err() {
                    break;
                }
            }
            Ok(_) => {}
            Err(err) => {
                warn!(
                    id = %active.id,
                    path = %active.output_dir.display(),
                    error = %err,
                    "live transcode segment scan failed"
                );
            }
        }
        sleep(SEGMENT_POLL_INTERVAL).await;
    }
}

async fn highest_segment(path: &Path) -> std::io::Result<Option<u64>> {
    let mut highest = None;
    let mut entries = tokio::fs::read_dir(path).await?;
    while let Some(entry) = entries.next_entry().await? {
        let filename = entry.file_name();
        let Some(filename) = filename.to_str() else {
            continue;
        };
        let Some(segment) = parse_segment_filename(filename) else {
            continue;
        };
        highest = Some(highest.map_or(segment, |value: u64| value.max(segment)));
    }
    Ok(highest)
}

fn parse_segment_filename(filename: &str) -> Option<u64> {
    let value = filename.strip_prefix("seg-")?.strip_suffix(".m4s")?;
    if value.len() != 5 || !value.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    value.parse().ok()
}

fn segment_path(output_dir: &Path, n: u64) -> PathBuf {
    output_dir.join(format!("seg-{n:05}.m4s"))
}

async fn directory_size(path: &Path) -> std::io::Result<u64> {
    let mut total = 0_u64;
    let mut stack = vec![path.to_path_buf()];

    while let Some(dir) = stack.pop() {
        let mut entries = tokio::fs::read_dir(&dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let metadata = entry.metadata().await?;
            if metadata.is_dir() {
                stack.push(entry.path());
            } else if metadata.is_file() {
                total = total.saturating_add(metadata.len());
            }
        }
    }

    Ok(total)
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use kino_core::Timestamp;
    use kino_db::Db;
    use tempfile::TempDir;
    use tokio::time::Duration;

    use super::*;
    use crate::{InputSpec, VideoCodec};

    #[tokio::test]
    async fn concurrent_live_requests_share_one_encode()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let source_file_id = insert_source_file(&db, "/library/source.mkv").await?;
        let temp = TempDir::new()?;
        let script_path = fake_ffmpeg_script(temp.path())?;
        let store = EphemeralStore::new(db);
        let active = ActiveEncodes::new(store, Arc::new(PipelineRunner::new()));
        let output_dir = temp.path().join("out");

        let request = || ActiveEncodeRequest {
            source_file_id,
            profile_hash: [9; 32],
            profile_json: "{}".to_owned(),
            output_dir: output_dir.clone(),
            command: FfmpegEncodeCommand::new(script_path.clone(), InputSpec::file("/tmp/source"))
                .video(crate::VideoOutputSpec {
                    codec: VideoCodec::Copy,
                    crf: None,
                    preset: crate::Preset::Medium,
                    bit_depth: 8,
                    color: crate::ColorOutput::CopyFromInput,
                    max_resolution: None,
                })
                .audio(crate::AudioPolicy::Copy)
                .hls(crate::HlsOutputSpec::cmaf_vod(
                    output_dir.clone(),
                    Duration::from_secs(6),
                )),
        };

        let first = active.get_or_spawn(request()).await?;
        let second = active.get_or_spawn(request()).await?;

        assert_eq!(first.id, second.id);
        assert_eq!(first.refcount.load(Ordering::Acquire), 2);
        Ok(())
    }

    fn fake_ffmpeg_script(dir: &Path) -> std::result::Result<PathBuf, Box<dyn std::error::Error>> {
        let script_path = dir.join("fake-ffmpeg");
        let script = r#"#!/bin/sh
set -eu
playlist=""
for arg do
  playlist="$arg"
done
out_dir=$(dirname "$playlist")
mkdir -p "$out_dir"
printf data > "$out_dir/init.mp4"
printf data > "$out_dir/seg-00000.m4s"
printf '#EXTM3U\n#EXT-X-MAP:URI="init.mp4"\n#EXTINF:1,\nseg-00000.m4s\n#EXT-X-ENDLIST\n' > "$playlist"
"#;
        fs::write(&script_path, script)?;
        make_executable(&script_path)?;
        Ok(script_path)
    }

    #[cfg(unix)]
    fn make_executable(path: &Path) -> std::result::Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = fs::metadata(path)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions)?;
        Ok(())
    }

    #[cfg(not(unix))]
    fn make_executable(_path: &Path) -> std::result::Result<(), Box<dyn std::error::Error>> {
        Ok(())
    }

    async fn insert_source_file(db: &Db, path: &str) -> std::result::Result<Id, sqlx::Error> {
        let media_item_id = Id::new();
        let source_file_id = Id::new();
        let now = Timestamp::now();

        sqlx::query(
            r#"
            INSERT INTO media_items (
                id,
                media_kind,
                canonical_identity_id,
                created_at,
                updated_at
            )
            VALUES (?1, 'personal', NULL, ?2, ?3)
            "#,
        )
        .bind(media_item_id)
        .bind(now)
        .bind(now)
        .execute(db.write_pool())
        .await?;

        sqlx::query(
            r#"
            INSERT INTO source_files (
                id,
                media_item_id,
                path,
                created_at,
                updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5)
            "#,
        )
        .bind(source_file_id)
        .bind(media_item_id)
        .bind(path)
        .bind(now)
        .bind(now)
        .execute(db.write_pool())
        .await?;

        Ok(source_file_id)
    }
}
