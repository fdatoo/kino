//! Resource-lane scheduler for durable transcode jobs.

use std::{path::PathBuf, sync::Arc, time::Duration};

use dashmap::DashMap;
use kino_core::Id;
use tokio::{sync::oneshot, task::JoinHandle, time::sleep};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use super::{JobState, JobStore, TranscodeJob};
use crate::encoder::SoftwareEncodeContext;
use crate::plan::{AudioPolicyKind, ColorTarget};
use crate::{
    AudioPolicy, ColorOutput, Encoder, EncoderRegistry, Error, HlsOutputSpec, LaneId,
    PipelineRunner, PlannedVariant, Preset, Result, VideoFilter, VideoOutputSpec, verify_outputs,
};

const DEFAULT_TICK_INTERVAL: Duration = Duration::from_millis(250);
const DEFAULT_MAX_ATTEMPTS: u32 = 3;
const DEFAULT_BACKOFF: Duration = Duration::from_secs(60);
const ALL_LANES: [LaneId; 3] = [LaneId::Cpu, LaneId::GpuIntel, LaneId::GpuVideoToolbox];

/// Scheduler runtime tuning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SchedulerConfig {
    /// Delay between dispatch ticks.
    pub tick_interval: Duration,
    /// Maximum recorded attempts before a retryable failure becomes terminal.
    pub max_attempts: u32,
    /// Delay before a retryable failed job becomes eligible again.
    pub backoff: Duration,
    /// Whether callers should run crash recovery before spawning the loop.
    pub recovery_on_boot: bool,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            tick_interval: DEFAULT_TICK_INTERVAL,
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            backoff: DEFAULT_BACKOFF,
            recovery_on_boot: true,
        }
    }
}

impl From<&kino_core::TranscodeSchedulerConfig> for SchedulerConfig {
    fn from(config: &kino_core::TranscodeSchedulerConfig) -> Self {
        Self {
            tick_interval: config.tick_interval,
            max_attempts: config.max_attempts,
            backoff: config.backoff,
            recovery_on_boot: config.recovery_on_boot,
        }
    }
}

/// Dispatches planned transcode jobs onto constrained encoder lanes.
pub struct Scheduler {
    store: JobStore,
    registry: Arc<EncoderRegistry>,
    runner: Arc<PipelineRunner>,
    tick_interval: Duration,
    max_attempts: u32,
    backoff: Duration,
    in_flight: Arc<DashMap<LaneId, JobInFlight>>,
    cancel_tokens: Arc<DashMap<Id, oneshot::Sender<()>>>,
    shutdown: CancellationToken,
}

#[derive(Debug)]
struct JobInFlight {
    job_id: Id,
}

impl Scheduler {
    /// Construct a scheduler over the provided store, encoder registry, and runner.
    pub fn new(
        store: JobStore,
        registry: Arc<EncoderRegistry>,
        runner: Arc<PipelineRunner>,
        config: SchedulerConfig,
    ) -> Self {
        let SchedulerConfig {
            tick_interval,
            max_attempts,
            backoff,
            recovery_on_boot: _,
        } = config;

        Self {
            store,
            registry,
            runner,
            tick_interval,
            max_attempts,
            backoff,
            in_flight: Arc::new(DashMap::new()),
            cancel_tokens: Arc::new(DashMap::new()),
            shutdown: CancellationToken::new(),
        }
    }

    /// Spawn the scheduler tick loop on the Tokio runtime.
    pub fn spawn(self: Arc<Self>) -> JoinHandle<()> {
        tokio::spawn(async move {
            self.run_loop().await;
        })
    }

    /// Signal the scheduler loop to stop dispatching new work.
    pub fn shutdown(&self) {
        self.shutdown.cancel();
    }

    /// Reset jobs left running by a previous process and return the affected count.
    pub async fn recover_on_boot(&self) -> Result<u64> {
        let count = self.store.reset_running_to_planned().await?;
        info!(count, "reset running transcode jobs on boot");
        Ok(count)
    }

    /// Cancel a running job, if this process owns its cancellation token.
    pub async fn cancel_job(&self, job_id: Id) -> Result<()> {
        if let Some((_, cancel)) = self.cancel_tokens.remove(&job_id)
            && cancel.send(()).is_err()
        {
            warn!(%job_id, "transcode job cancellation receiver was already closed");
        }

        transition_or_accept_terminal(
            &self.store,
            job_id,
            JobState::Running,
            JobState::Cancelled,
            None,
        )
        .await?;
        Ok(())
    }

    async fn run_loop(self: Arc<Self>) {
        loop {
            if self.shutdown.is_cancelled() {
                break;
            }

            if let Err(err) = self.dispatch_tick().await {
                error!(error = %err, "transcode scheduler tick failed");
            }

            tokio::select! {
                () = self.shutdown.cancelled() => break,
                () = sleep(self.tick_interval) => {}
            }
        }
    }

    async fn dispatch_tick(self: &Arc<Self>) -> Result<()> {
        for lane in ALL_LANES {
            if self.registry.by_lane(lane).next().is_none() || self.in_flight.contains_key(&lane) {
                continue;
            }

            let Some(job) = self.store.claim_next_for_lane(lane).await? else {
                continue;
            };

            self.dispatch_job(job).await?;
        }

        Ok(())
    }

    async fn dispatch_job(self: &Arc<Self>, job: TranscodeJob) -> Result<()> {
        let variant = match serde_json::from_str::<PlannedVariant>(&job.profile_json) {
            Ok(variant) => variant,
            Err(err) => {
                self.fail_running_job(job.id, err.to_string()).await?;
                return Ok(());
            }
        };
        let (width, height) = variant_dimensions(&variant);
        let Some(encoder) = self.select_encoder(job.lane, &variant, width, height) else {
            self.fail_running_job(job.id, "no encoder supports this variant")
                .await?;
            return Ok(());
        };

        let context = match encode_context(&job, &variant, width, height) {
            Ok(context) => context,
            Err(err) => {
                self.fail_running_job(job.id, err.to_string()).await?;
                return Ok(());
            }
        };
        let command = match encoder.build_command(&context) {
            Ok(command) => command,
            Err(err) => {
                self.fail_running_job(job.id, err.to_string()).await?;
                return Ok(());
            }
        };
        let output_dir = context.hls.output_dir.clone();
        let (cancel_tx, cancel_rx) = oneshot::channel();

        self.cancel_tokens.insert(job.id, cancel_tx);
        self.in_flight
            .insert(job.lane, JobInFlight { job_id: job.id });

        let scheduler = Arc::clone(self);
        tokio::spawn(async move {
            scheduler.run_job(job, command, output_dir, cancel_rx).await;
        });

        Ok(())
    }

    fn select_encoder(
        &self,
        lane: LaneId,
        variant: &PlannedVariant,
        width: u32,
        height: u32,
    ) -> Option<&dyn Encoder> {
        self.registry
            .select_for_codec(variant.codec, width, height, variant.bit_depth)
            .filter(|encoder| encoder.lane() == lane)
            .or_else(|| {
                self.registry.by_lane(lane).find(|encoder| {
                    encoder.supports_codec(variant.codec, width, height, variant.bit_depth)
                })
            })
    }

    async fn run_job(
        self: Arc<Self>,
        job: TranscodeJob,
        command: crate::FfmpegEncodeCommand,
        output_dir: PathBuf,
        cancel_rx: oneshot::Receiver<()>,
    ) {
        let result = self.runner.run(command, cancel_rx).await;
        match result {
            Ok(_) => {
                if let Err(err) = self.complete_job(job.id, &output_dir).await {
                    error!(%job.id, error = %err, "transcode job completion failed");
                }
            }
            Err(Error::Cancelled) => {
                if let Err(err) = transition_or_accept_terminal(
                    &self.store,
                    job.id,
                    JobState::Running,
                    JobState::Cancelled,
                    None,
                )
                .await
                {
                    error!(%job.id, error = %err, "transcode job cancellation transition failed");
                }
            }
            Err(err) => {
                if let Err(update_err) = self.handle_job_error(&job, &err).await {
                    error!(%job.id, error = %update_err, "transcode job failure update failed");
                }
            }
        }

        self.cleanup_job(job.lane, job.id);
    }

    async fn complete_job(&self, job_id: Id, output_dir: &std::path::Path) -> Result<()> {
        self.store
            .transition_state(job_id, JobState::Running, JobState::Verifying, None)
            .await?;
        if let Err(err) = verify_outputs(output_dir) {
            self.store
                .transition_state(
                    job_id,
                    JobState::Verifying,
                    JobState::Failed,
                    Some(&err.to_string()),
                )
                .await?;
            return Err(err);
        }
        self.store
            .transition_state(job_id, JobState::Verifying, JobState::Completed, None)
            .await?;
        Ok(())
    }

    async fn handle_job_error(&self, job: &TranscodeJob, err: &Error) -> Result<()> {
        let message = err.to_string();
        if err.is_transient() && job.attempt < self.max_attempts {
            self.store
                .reset_for_retry(job.id, &message, self.backoff)
                .await?;
        } else {
            self.store
                .transition_state(job.id, JobState::Running, JobState::Failed, Some(&message))
                .await?;
        }
        Ok(())
    }

    async fn fail_running_job(&self, job_id: Id, error: impl AsRef<str>) -> Result<()> {
        self.store
            .transition_state(
                job_id,
                JobState::Running,
                JobState::Failed,
                Some(error.as_ref()),
            )
            .await?;
        Ok(())
    }

    fn cleanup_job(&self, lane: LaneId, job_id: Id) {
        self.cancel_tokens.remove(&job_id);
        if self
            .in_flight
            .get(&lane)
            .is_some_and(|entry| entry.job_id == job_id)
        {
            self.in_flight.remove(&lane);
        }
    }
}

async fn transition_or_accept_terminal(
    store: &JobStore,
    job_id: Id,
    from: JobState,
    to: JobState,
    error: Option<&str>,
) -> Result<()> {
    match store.transition_state(job_id, from, to, error).await {
        Ok(_) => Ok(()),
        Err(Error::JobNotFound { id }) if id == job_id => {
            let job = store.fetch_job(job_id).await?;
            if job.state == to {
                Ok(())
            } else {
                Err(Error::JobNotFound { id })
            }
        }
        Err(err) => Err(err),
    }
}

fn encode_context(
    job: &TranscodeJob,
    variant: &PlannedVariant,
    width: u32,
    height: u32,
) -> Result<SoftwareEncodeContext> {
    let output_dir = std::env::temp_dir()
        .join("kino-transcode")
        .join("jobs")
        .join(job.id.to_string());
    std::fs::create_dir_all(&output_dir)?;

    Ok(SoftwareEncodeContext {
        input_path: PathBuf::from("/kino/source-path-unavailable"),
        video: VideoOutputSpec {
            codec: variant.codec,
            crf: variant.vmaf_target.map(|_| 23),
            preset: Preset::Medium,
            bit_depth: variant.bit_depth,
            color: match variant.color {
                ColorTarget::Sdr => ColorOutput::SdrBt709,
                ColorTarget::Hdr10 => ColorOutput::CopyFromInput,
            },
            max_resolution: Some((width, height)),
        },
        audio: match variant.audio {
            AudioPolicyKind::StereoAac => AudioPolicy::StereoAac { bitrate_kbps: 192 },
            AudioPolicyKind::StereoAacWithSurroundPassthrough => {
                AudioPolicy::StereoAacWithSurroundPassthrough { bitrate_kbps: 192 }
            }
            AudioPolicyKind::Copy => AudioPolicy::Copy,
        },
        filters: variant
            .width
            .map(|planned_width| vec![VideoFilter::Scale(planned_width, height)])
            .unwrap_or_default(),
        hls: HlsOutputSpec::cmaf_vod(output_dir, Duration::from_secs(6)),
    })
}

fn variant_dimensions(variant: &PlannedVariant) -> (u32, u32) {
    let width = variant.width.unwrap_or(1920);
    let height = width.saturating_mul(9).div_ceil(16).max(1);
    (width, height)
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path, sync::Arc, time::Duration};

    use kino_core::{Id, Timestamp};
    use kino_db::Db;
    use tempfile::TempDir;

    use super::*;
    use crate::{
        Capabilities, EncoderKind, InputSpec, NewJob, VideoCodec,
        plan::variant::{AudioPolicyKind, Container, VariantKind},
    };

    #[tokio::test]
    async fn empty_planned_queue_tick_does_nothing()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let fixture = Fixture::new([fake_encoder(LaneId::Cpu, VideoCodec::H264, 25)?]).await?;
        let scheduler = fixture.scheduler();

        scheduler.dispatch_tick().await?;

        assert!(scheduler.in_flight.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn single_planned_job_on_cpu_completes()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let fixture = Fixture::new([fake_encoder(LaneId::Cpu, VideoCodec::H264, 25)?]).await?;
        let job_id = fixture.insert_job(1, LaneId::Cpu, VideoCodec::H264).await?;
        let scheduler = fixture.scheduler();
        let handle = Arc::clone(&scheduler).spawn();

        let completed = wait_for_state(&fixture.store, job_id, JobState::Completed).await?;

        scheduler.shutdown();
        handle.await?;
        assert_eq!(completed.attempt, 1);
        Ok(())
    }

    #[tokio::test]
    async fn same_lane_jobs_run_one_at_a_time()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let fixture = Fixture::new([fake_encoder(LaneId::Cpu, VideoCodec::H264, 300)?]).await?;
        let first_id = fixture.insert_job(1, LaneId::Cpu, VideoCodec::H264).await?;
        let second_id = fixture.insert_job(2, LaneId::Cpu, VideoCodec::H264).await?;
        let scheduler = fixture.scheduler();
        let handle = Arc::clone(&scheduler).spawn();

        let first_running = wait_for_state(&fixture.store, first_id, JobState::Running).await?;
        let second_before = fixture.store.fetch_job(second_id).await?;
        assert_eq!(first_running.state, JobState::Running);
        assert_eq!(second_before.state, JobState::Planned);

        wait_for_state(&fixture.store, first_id, JobState::Completed).await?;
        wait_for_state(&fixture.store, second_id, JobState::Completed).await?;

        scheduler.shutdown();
        handle.await?;
        Ok(())
    }

    #[tokio::test]
    async fn different_lane_jobs_run_concurrently()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let fixture = Fixture::new([
            fake_encoder(LaneId::Cpu, VideoCodec::H264, 300)?,
            fake_encoder(LaneId::GpuIntel, VideoCodec::Hevc, 300)?,
        ])
        .await?;
        let cpu_id = fixture.insert_job(1, LaneId::Cpu, VideoCodec::H264).await?;
        let gpu_id = fixture
            .insert_job(2, LaneId::GpuIntel, VideoCodec::Hevc)
            .await?;
        let scheduler = fixture.scheduler();
        let handle = Arc::clone(&scheduler).spawn();

        wait_for_state(&fixture.store, cpu_id, JobState::Running).await?;
        wait_for_state(&fixture.store, gpu_id, JobState::Running).await?;
        wait_for_state(&fixture.store, cpu_id, JobState::Completed).await?;
        wait_for_state(&fixture.store, gpu_id, JobState::Completed).await?;

        scheduler.shutdown();
        handle.await?;
        Ok(())
    }

    #[tokio::test]
    async fn cancellation_frees_lane_for_next_job()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let fixture = Fixture::new([fake_encoder(LaneId::Cpu, VideoCodec::H264, 2_000)?]).await?;
        let first_id = fixture.insert_job(1, LaneId::Cpu, VideoCodec::H264).await?;
        let second_id = fixture.insert_job(2, LaneId::Cpu, VideoCodec::H264).await?;
        let scheduler = fixture.scheduler();
        let handle = Arc::clone(&scheduler).spawn();

        wait_for_state(&fixture.store, first_id, JobState::Running).await?;
        sleep(Duration::from_millis(100)).await;
        scheduler.cancel_job(first_id).await?;
        wait_for_state(&fixture.store, first_id, JobState::Cancelled).await?;
        wait_for_state(&fixture.store, second_id, JobState::Running).await?;
        scheduler.cancel_job(second_id).await?;
        wait_for_state(&fixture.store, second_id, JobState::Cancelled).await?;

        scheduler.shutdown();
        handle.await?;
        Ok(())
    }

    #[tokio::test]
    async fn crash_recovery_resets_running_jobs()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let fixture = Fixture::new([fake_encoder(LaneId::Cpu, VideoCodec::H264, 25)?]).await?;
        let job_id = fixture.insert_job(1, LaneId::Cpu, VideoCodec::H264).await?;
        let _ = fixture.store.claim_next_for_lane(LaneId::Cpu).await?;
        let scheduler = fixture.scheduler();

        let count = scheduler.recover_on_boot().await?;
        let job = fixture.store.fetch_job(job_id).await?;

        assert_eq!(count, 1);
        assert_eq!(job.state, JobState::Planned);
        assert_eq!(job.attempt, 2);
        Ok(())
    }

    struct Fixture {
        db: Db,
        store: JobStore,
        registry: Arc<EncoderRegistry>,
    }

    impl Fixture {
        async fn new<const N: usize>(
            encoders: [FakeEncoder; N],
        ) -> std::result::Result<Self, Box<dyn std::error::Error>> {
            let db = kino_db::test_db().await?;
            let store = JobStore::new(db.clone());
            let registry = Arc::new(EncoderRegistry::from_encoders(
                encoders
                    .into_iter()
                    .map(|encoder| Box::new(encoder) as Box<dyn Encoder>)
                    .collect(),
            ));

            Ok(Self {
                db,
                store,
                registry,
            })
        }

        fn scheduler(&self) -> Arc<Scheduler> {
            Arc::new(Scheduler::new(
                self.store.clone(),
                Arc::clone(&self.registry),
                Arc::new(PipelineRunner::new()),
                SchedulerConfig {
                    tick_interval: Duration::from_millis(25),
                    max_attempts: 3,
                    backoff: Duration::from_millis(1),
                    recovery_on_boot: true,
                },
            ))
        }

        async fn insert_job(&self, seed: u8, lane: LaneId, codec: VideoCodec) -> Result<Id> {
            let source_file_id =
                insert_source_file(&self.db, &format!("/library/{seed}.mkv")).await?;
            let variant = planned_variant(codec);
            let profile_json = serde_json::to_string(&variant)?;
            let Some(id) = self
                .store
                .insert_job_idempotent(&NewJob {
                    source_file_id,
                    profile_json,
                    profile_hash: [seed; 32],
                    lane,
                })
                .await?
            else {
                return Err(Error::JobNotFound { id: source_file_id });
            };
            Ok(id)
        }
    }

    struct FakeEncoder {
        lane: LaneId,
        kind: EncoderKind,
        capabilities: Capabilities,
        _temp_dir: TempDir,
        script_path: PathBuf,
    }

    impl Encoder for FakeEncoder {
        fn kind(&self) -> EncoderKind {
            self.kind
        }

        fn lane(&self) -> LaneId {
            self.lane
        }

        fn capabilities(&self) -> &Capabilities {
            &self.capabilities
        }

        fn supports_codec(
            &self,
            codec: VideoCodec,
            width: u32,
            height: u32,
            bit_depth: u8,
        ) -> bool {
            self.capabilities.codecs().contains(&codec)
                && width <= self.capabilities.max_width()
                && height <= self.capabilities.max_height()
                && (bit_depth <= 8 || self.capabilities.ten_bit())
        }

        fn build_command(&self, ctx: &SoftwareEncodeContext) -> Result<crate::FfmpegEncodeCommand> {
            Ok(crate::FfmpegEncodeCommand::new(
                self.script_path.clone(),
                InputSpec::file(ctx.input_path.clone()),
            )
            .video(ctx.video.clone())
            .audio(ctx.audio.clone())
            .hls(ctx.hls.clone()))
        }
    }

    fn fake_encoder(
        lane: LaneId,
        codec: VideoCodec,
        sleep_ms: u64,
    ) -> std::result::Result<FakeEncoder, Box<dyn std::error::Error>> {
        let temp_dir = TempDir::new()?;
        let script_path = temp_dir.path().join("fake-ffmpeg");
        let script = format!(
            r#"#!/bin/sh
set -eu
/bin/sleep {}
playlist=""
for arg do
  playlist="$arg"
done
dir=$(dirname "$playlist")
mkdir -p "$dir"
printf data > "$dir/init.mp4"
printf data > "$dir/seg-00000.m4s"
printf '#EXTM3U\n#EXTINF:1,\nseg-00000.m4s\n#EXT-X-ENDLIST\n' > "$playlist"
"#,
            sleep_ms as f64 / 1000.0
        );
        fs::write(&script_path, script)?;
        make_executable(&script_path)?;

        Ok(FakeEncoder {
            lane,
            kind: match lane {
                LaneId::Cpu => EncoderKind::Software,
                LaneId::GpuIntel => EncoderKind::Qsv,
                LaneId::GpuVideoToolbox => EncoderKind::VideoToolbox,
            },
            capabilities: Capabilities::new([codec], 3840, 2160, true, true, true),
            _temp_dir: temp_dir,
            script_path,
        })
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

    fn planned_variant(codec: VideoCodec) -> PlannedVariant {
        PlannedVariant {
            kind: VariantKind::Compatibility,
            codec,
            container: Container::Fmp4Cmaf,
            width: Some(1920),
            bit_depth: 8,
            color: ColorTarget::Sdr,
            audio: AudioPolicyKind::StereoAac,
            vmaf_target: Some(90.0),
        }
    }

    async fn wait_for_state(store: &JobStore, job_id: Id, state: JobState) -> Result<TranscodeJob> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let job = store.fetch_job(job_id).await?;
            if job.state == state {
                return Ok(job);
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(Error::InvalidJobState(format!(
                    "timed out waiting for {state}; saw {}",
                    job.state
                )));
            }
            sleep(Duration::from_millis(10)).await;
        }
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
