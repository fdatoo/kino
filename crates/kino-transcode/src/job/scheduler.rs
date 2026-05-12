//! Resource-lane scheduler for durable transcode jobs.

use std::{path::PathBuf, sync::Arc, time::Duration};

use dashmap::DashMap;
use kino_core::Id;
use tokio::{sync::oneshot, task::JoinHandle, time::sleep};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use super::{JobState, JobStore, NewTranscodeOutput, TranscodeJob};
use crate::encoder::SoftwareEncodeContext;
use crate::plan::{AudioPolicyKind, ColorTarget};
use crate::{
    AudioPolicy, ColorDowngrade, ColorOutput, DowngradeStore, EncodeMetadata, Encoder,
    EncoderRegistry, Error, HlsOutputSpec, LaneId, PipelineRunner, PlannedVariant, Preset, Result,
    SampleMeasurement, TranscodeProfile, VideoFilter, VideoOutputSpec, VideoRange, verify_outputs,
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

        let job = self.store.fetch_job(job_id).await?;
        transition_or_accept_terminal(&self.store, job_id, job.state, JobState::Cancelled, None)
            .await?;
        Ok(())
    }

    /// Return the lane selected for a planned variant on this host.
    pub fn lane_for_variant(&self, variant: &PlannedVariant) -> Result<LaneId> {
        let (width, height) = variant_dimensions(variant);
        self.registry
            .select_for_codec(variant.codec, width, height, variant.bit_depth)
            .map(Encoder::lane)
            .ok_or(Error::NoEncoderForVariant {
                codec: variant.codec,
                width,
                height,
                bit_depth: variant.bit_depth,
            })
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
        let planned = match planned_job(&job) {
            Ok(planned) => planned,
            Err(err) => {
                self.fail_running_job(job.id, err.to_string()).await?;
                return Ok(());
            }
        };
        let variant = planned.variant;
        let (width, height) = variant_dimensions(&variant);
        let Some(encoder) = self.select_encoder(job.lane, &variant, width, height) else {
            self.fail_running_job(job.id, "no encoder supports this variant")
                .await?;
            return Ok(());
        };
        let input_path = match self.store.source_path(job.source_file_id).await {
            Ok(path) => path,
            Err(err) => {
                self.fail_running_job(job.id, err.to_string()).await?;
                return Ok(());
            }
        };

        let context =
            match encode_context(&job, &variant, &planned.source, input_path, width, height) {
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
                if let Err(err) = self.complete_job(&job, &output_dir).await {
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

    async fn complete_job(&self, job: &TranscodeJob, output_dir: &std::path::Path) -> Result<()> {
        self.store
            .transition_state(job.id, JobState::Running, JobState::Verifying, None)
            .await?;
        if let Err(err) = verify_outputs(output_dir) {
            self.store
                .transition_state(
                    job.id,
                    JobState::Verifying,
                    JobState::Failed,
                    Some(&err.to_string()),
                )
                .await?;
            return Err(err);
        }
        persist_output(&self.store, job, output_dir).await?;
        self.store
            .transition_state(job.id, JobState::Verifying, JobState::Completed, None)
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
    source: &SourceColorContext,
    input_path: PathBuf,
    width: u32,
    height: u32,
) -> Result<SoftwareEncodeContext> {
    let output_dir = std::env::temp_dir()
        .join("kino-transcode")
        .join("jobs")
        .join(job.id.to_string());
    std::fs::create_dir_all(&output_dir)?;
    let copy = variant.codec == crate::VideoCodec::Copy;

    Ok(SoftwareEncodeContext {
        input_path,
        video: VideoOutputSpec {
            codec: variant.codec,
            crf: (!copy).then(|| variant.vmaf_target.map(|_| 23)).flatten(),
            preset: Preset::Medium,
            bit_depth: variant.bit_depth,
            color: color_output(variant, source),
            max_resolution: (!copy).then_some((width, height)),
        },
        audio: match variant.audio {
            AudioPolicyKind::StereoAac => AudioPolicy::StereoAac { bitrate_kbps: 192 },
            AudioPolicyKind::StereoAacWithSurroundPassthrough => {
                AudioPolicy::StereoAacWithSurroundPassthrough { bitrate_kbps: 192 }
            }
            AudioPolicyKind::Copy => AudioPolicy::Copy,
        },
        filters: video_filters(variant, source, height),
        hls: HlsOutputSpec::cmaf_vod(output_dir, Duration::from_secs(6)),
    })
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct SourceColorContext {
    color_transfer: Option<String>,
    master_display: Option<kino_core::MasterDisplay>,
    max_cll: Option<kino_core::MaxCll>,
    dolby_vision: Option<kino_core::DolbyVision>,
}

#[derive(Debug, Clone, PartialEq)]
struct PlannedJob {
    variant: PlannedVariant,
    source: SourceColorContext,
    duration_us: Option<u64>,
}

fn planned_job(job: &TranscodeJob) -> Result<PlannedJob> {
    if let Ok(profile) = serde_json::from_str::<TranscodeProfile>(&job.profile_json) {
        return Ok(PlannedJob {
            variant: profile.variant(),
            source: SourceColorContext {
                color_transfer: profile.source_color_transfer,
                master_display: profile.source_master_display,
                max_cll: profile.source_max_cll,
                dolby_vision: profile.source_dolby_vision,
            },
            duration_us: profile.source_duration_us,
        });
    }

    Ok(PlannedJob {
        variant: serde_json::from_str::<PlannedVariant>(&job.profile_json)?,
        source: SourceColorContext::default(),
        duration_us: None,
    })
}

fn color_output(variant: &PlannedVariant, source: &SourceColorContext) -> ColorOutput {
    if variant.codec == crate::VideoCodec::Copy {
        return ColorOutput::CopyFromInput;
    }

    match variant.color {
        ColorTarget::Sdr => ColorOutput::SdrBt709,
        ColorTarget::Hdr10 => match (source.master_display.clone(), source.max_cll.clone()) {
            (Some(master_display), Some(max_cll)) => ColorOutput::Hdr10 {
                master_display,
                max_cll,
            },
            _ => ColorOutput::CopyFromInput,
        },
    }
}

fn video_filters(
    variant: &PlannedVariant,
    source: &SourceColorContext,
    height: u32,
) -> Vec<VideoFilter> {
    if variant.codec == crate::VideoCodec::Copy {
        return Vec::new();
    }

    let mut filters = Vec::new();
    if variant.color == ColorTarget::Sdr && source_is_hdr_or_dv(source) {
        filters.push(VideoFilter::HdrToSdrTonemap);
    }
    if let Some(planned_width) = variant.width {
        filters.push(VideoFilter::Scale(planned_width, height));
    }
    filters
}

fn source_is_hdr_or_dv(source: &SourceColorContext) -> bool {
    source.dolby_vision.is_some()
        || source.master_display.is_some()
        || source
            .color_transfer
            .as_deref()
            .is_some_and(|transfer| matches!(transfer, "smpte2084" | "arib-std-b67"))
}

fn detect_color_downgrade(
    variant: &PlannedVariant,
    source: &SourceColorContext,
) -> Option<ColorDowngrade> {
    if variant.codec == crate::VideoCodec::Copy {
        return None;
    }

    if source.dolby_vision.is_some() {
        return match variant.color {
            ColorTarget::Hdr10 => Some(ColorDowngrade::DvToHdr10),
            ColorTarget::Sdr => Some(ColorDowngrade::DvToSdr),
        };
    }

    (variant.color == ColorTarget::Sdr && source_is_hdr_or_dv(source))
        .then_some(ColorDowngrade::Hdr10ToSdr)
}

async fn persist_output(
    store: &JobStore,
    job: &TranscodeJob,
    output_dir: &std::path::Path,
) -> Result<()> {
    let planned = planned_job(job)?;
    let (width, height) = variant_dimensions(&planned.variant);
    let downgrade = detect_color_downgrade(&planned.variant, &planned.source);
    let metadata = EncodeMetadata {
        encoder_kind: job.lane.as_str().to_owned(),
        ffmpeg_version: None,
        duration_us: planned.duration_us.unwrap_or(0),
        vmaf_samples: Vec::<SampleMeasurement>::new(),
        chosen_crf: planned.variant.vmaf_target.map(|_| 23),
        spot_check_vmaf: None,
        codecs: codecs_for_variant(&planned.variant),
        resolution: (width, height),
        bandwidth: None,
        video_range: video_range_for_variant(&planned.variant),
        color_downgrade: downgrade,
    };
    let hls = HlsOutputSpec::cmaf_vod(output_dir.to_path_buf(), Duration::from_secs(6));
    let output_id = store
        .upsert_transcode_output(&NewTranscodeOutput {
            source_file_id: job.source_file_id,
            path: hls
                .output_dir
                .join(&hls.playlist_filename)
                .display()
                .to_string(),
            directory_path: hls.output_dir.display().to_string(),
            playlist_filename: hls.playlist_filename,
            init_filename: hls.init_filename,
            encode_metadata_json: serde_json::to_string(&metadata)?,
        })
        .await?;

    if let Some(kind) = downgrade {
        DowngradeStore::new(store.db().clone())
            .insert_color_downgrade(output_id, kind, None)
            .await?;
    }

    Ok(())
}

fn codecs_for_variant(variant: &PlannedVariant) -> String {
    match variant.codec {
        crate::VideoCodec::Hevc | crate::VideoCodec::Copy => "hvc1,mp4a.40.2",
        crate::VideoCodec::H264 => "avc1.640028,mp4a.40.2",
        crate::VideoCodec::Av1 => "av01,mp4a.40.2",
    }
    .to_owned()
}

fn video_range_for_variant(variant: &PlannedVariant) -> VideoRange {
    match variant.color {
        ColorTarget::Hdr10 => VideoRange::Pq,
        ColorTarget::Sdr => VideoRange::Sdr,
    }
}

fn variant_dimensions(variant: &PlannedVariant) -> (u32, u32) {
    let width = variant.width.unwrap_or(1920);
    let height = width.saturating_mul(9).div_ceil(16).max(1);
    (width, height)
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path, sync::Arc, time::Duration};

    use kino_core::{
        DolbyVision, Id, MasterDisplay, MaxCll, ProbeResult, ProbeVideoStream, Timestamp,
    };
    use kino_db::Db;
    use tempfile::TempDir;

    use super::*;
    use crate::{
        Capabilities, EncoderKind, InputSpec, NewJob, SourceContext, VideoCodec,
        encoder::SoftwareEncoder,
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

    #[test]
    fn snapshot_dv_original_copy_maps_all_streams_without_color_overrides() {
        let source = source_context_for_video(dolby_vision_video());
        let variant = PlannedVariant {
            kind: VariantKind::Original,
            codec: VideoCodec::Copy,
            container: Container::Fmp4Cmaf,
            width: None,
            bit_depth: 10,
            color: ColorTarget::Hdr10,
            audio: AudioPolicyKind::Copy,
            vmaf_target: None,
        };
        let command = command_for_variant(&source, &variant, 3840, 2160);

        insta::assert_snapshot!(format!("{command}"));
    }

    #[test]
    fn snapshot_hdr10_high_reencode_preserves_hdr10_metadata() {
        let source = source_context_for_video(hdr10_video());
        let variant = PlannedVariant {
            kind: VariantKind::High,
            codec: VideoCodec::Hevc,
            container: Container::Fmp4Cmaf,
            width: None,
            bit_depth: 10,
            color: ColorTarget::Hdr10,
            audio: AudioPolicyKind::StereoAacWithSurroundPassthrough,
            vmaf_target: Some(95.0),
        };
        let command = command_for_variant(&source, &variant, 3840, 2160);

        insta::assert_snapshot!(format!("{command}"));
    }

    #[test]
    fn snapshot_hdr10_compatibility_reencode_tonemaps_to_sdr() {
        let source = source_context_for_video(hdr10_video());
        let variant = PlannedVariant {
            kind: VariantKind::Compatibility,
            codec: VideoCodec::H264,
            container: Container::Fmp4Cmaf,
            width: Some(1920),
            bit_depth: 8,
            color: ColorTarget::Sdr,
            audio: AudioPolicyKind::StereoAac,
            vmaf_target: Some(90.0),
        };
        let command = command_for_variant(&source, &variant, 1920, 1080);

        insta::assert_snapshot!(format!("{command}"));
    }

    #[test]
    fn color_downgrade_detection_classifies_dv_and_hdr10_outputs() {
        let mut variant = planned_variant(VideoCodec::Hevc);
        variant.color = ColorTarget::Hdr10;
        let dv = source_context_for_video(dolby_vision_video());
        let hdr10 = source_context_for_video(hdr10_video());
        let dv_profile = TranscodeProfile::from_source_variant(&dv, &variant);
        let dv_source = SourceColorContext {
            color_transfer: dv_profile.source_color_transfer,
            master_display: dv_profile.source_master_display,
            max_cll: dv_profile.source_max_cll,
            dolby_vision: dv_profile.source_dolby_vision,
        };

        assert_eq!(
            detect_color_downgrade(&variant, &dv_source),
            Some(ColorDowngrade::DvToHdr10)
        );

        variant.color = ColorTarget::Sdr;
        let hdr10_profile = TranscodeProfile::from_source_variant(&hdr10, &variant);
        let hdr10_source = SourceColorContext {
            color_transfer: hdr10_profile.source_color_transfer,
            master_display: hdr10_profile.source_master_display,
            max_cll: hdr10_profile.source_max_cll,
            dolby_vision: hdr10_profile.source_dolby_vision,
        };
        assert_eq!(
            detect_color_downgrade(&variant, &hdr10_source),
            Some(ColorDowngrade::Hdr10ToSdr)
        );

        let dv_profile = TranscodeProfile::from_source_variant(&dv, &variant);
        let dv_source = SourceColorContext {
            color_transfer: dv_profile.source_color_transfer,
            master_display: dv_profile.source_master_display,
            max_cll: dv_profile.source_max_cll,
            dolby_vision: dv_profile.source_dolby_vision,
        };
        assert_eq!(
            detect_color_downgrade(&variant, &dv_source),
            Some(ColorDowngrade::DvToSdr)
        );
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

    fn command_for_variant(
        source: &SourceContext,
        variant: &PlannedVariant,
        width: u32,
        height: u32,
    ) -> crate::FfmpegEncodeCommand {
        let profile = TranscodeProfile::from_source_variant(source, variant);
        let job = transcode_job(profile.profile_json());
        let planned = match planned_job(&job) {
            Ok(planned) => planned,
            Err(err) => panic!("profile should decode: {err}"),
        };
        let mut context = match encode_context(
            &job,
            &planned.variant,
            &planned.source,
            source.probe.source_path.clone(),
            width,
            height,
        ) {
            Ok(context) => context,
            Err(err) => panic!("context should build: {err}"),
        };
        context.hls = HlsOutputSpec::cmaf_vod(
            format!(
                "/library/Some Movie/transcodes/{}",
                planned.variant.kind.as_str()
            ),
            Duration::from_secs(6),
        );

        SoftwareEncoder::new().build_command(&context)
    }

    fn transcode_job(profile_json: String) -> TranscodeJob {
        let now = Timestamp::now();
        TranscodeJob {
            id: fixed_id("018f16f2-76c0-7c5d-9a38-6dc365f4f062"),
            source_file_id: fixed_id("018f16f2-76c0-7c5d-9a38-6dc365f4f063"),
            profile_json,
            profile_hash: [7; 32],
            state: JobState::Running,
            lane: LaneId::Cpu,
            attempt: 1,
            progress_pct: None,
            last_error: None,
            next_attempt_at: None,
            created_at: now,
            updated_at: now,
            started_at: Some(now),
            completed_at: None,
        }
    }

    fn source_context_for_video(video: ProbeVideoStream) -> SourceContext {
        SourceContext {
            source_file_id: fixed_id("018f16f2-76c0-7c5d-9a38-6dc365f4f063"),
            probe: ProbeResult {
                source_path: PathBuf::from("/library/Some Movie/source.mkv"),
                container: None,
                title: None,
                duration: None,
                video_streams: vec![video],
                audio_streams: Vec::new(),
                subtitle_streams: Vec::new(),
            },
        }
    }

    fn hdr10_video() -> ProbeVideoStream {
        let mut video = base_video();
        video.color_primaries = Some("bt2020".to_owned());
        video.color_transfer = Some("smpte2084".to_owned());
        video.color_space = Some("bt2020nc".to_owned());
        video.master_display = Some(master_display());
        video.max_cll = Some(MaxCll {
            max_content: 1_000,
            max_average: 400,
        });
        video
    }

    fn dolby_vision_video() -> ProbeVideoStream {
        let mut video = hdr10_video();
        video.dolby_vision = Some(DolbyVision {
            profile: 8,
            level: 6,
            rpu_present: true,
            el_present: false,
            bl_present: true,
        });
        video
    }

    fn base_video() -> ProbeVideoStream {
        ProbeVideoStream {
            index: 0,
            codec_name: Some("hevc".to_owned()),
            codec_long_name: None,
            width: Some(3840),
            height: Some(2160),
            language: None,
            color_primaries: Some("bt709".to_owned()),
            color_transfer: Some("bt709".to_owned()),
            color_space: Some("bt709".to_owned()),
            master_display: None,
            max_cll: None,
            dolby_vision: None,
        }
    }

    const fn master_display() -> MasterDisplay {
        MasterDisplay {
            red_x: 34_000,
            red_y: 16_000,
            green_x: 13_250,
            green_y: 34_500,
            blue_x: 7_500,
            blue_y: 3_000,
            white_x: 15_635,
            white_y: 16_450,
            min_luminance: 50,
            max_luminance: 10_000_000,
        }
    }

    fn fixed_id(value: &str) -> Id {
        match value.parse() {
            Ok(id) => id,
            Err(err) => panic!("test id should parse: {err}"),
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
