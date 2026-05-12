//! VMAF sample selection, measurement aggregation, curve fitting, and metadata.

use std::{collections::BTreeMap, path::PathBuf, time::Duration};

use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

use super::SourceContext;
use crate::{
    Encoder, Error, FfmpegEncodeCommand, FfmpegVmafCommand, InputSpec, PipelineRunner, Result,
    TranscodeFuture, VideoFilter, VideoOutputSpec,
};

/// One VMAF trial measurement for a source sample and CRF.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SampleMeasurement {
    /// Zero-based sample index in the selected sample list.
    pub sample_idx: usize,
    /// Trial CRF used to encode the distorted sample.
    pub crf: u8,
    /// Pooled mean VMAF reported by libvmaf.
    pub mean_vmaf: f32,
}

/// Configuration for sample-based CRF measurement.
#[derive(Debug, Clone, PartialEq)]
pub struct VmafSamplingConfig {
    /// Number of source samples requested before short-source reduction.
    pub sample_count: usize,
    /// Duration of each source sample in seconds.
    pub sample_seconds: u32,
    /// Trial CRFs measured for each selected sample.
    pub trial_crfs: Vec<u8>,
    /// Directory for temporary distorted samples and libvmaf JSON logs.
    pub work_dir: PathBuf,
    /// FFmpeg binary used for libvmaf measurement.
    pub ffmpeg_binary: PathBuf,
    /// Video output settings shared with the main encode, with CRF replaced per trial.
    pub video: VideoOutputSpec,
    /// Video filters shared with the main encode.
    pub filters: Vec<VideoFilter>,
}

/// Request an encoder uses to build one distorted sample encode command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmafTrialEncodeRequest {
    /// Zero-based sample index in the selected sample list.
    pub sample_idx: usize,
    /// Trial CRF used for this distorted sample.
    pub crf: u8,
    /// Trimmed source input rendered with `-ss` and `-t`.
    pub input: InputSpec,
    /// Distorted sample path the encode command must produce.
    pub output_path: PathBuf,
    /// Video output settings with the trial CRF applied.
    pub video: VideoOutputSpec,
    /// Video filters shared with the main encode.
    pub filters: Vec<VideoFilter>,
}

/// Encoder extension used to build VMAF trial sample encodes.
pub trait VmafSampleEncoder: Encoder {
    /// Build the FFmpeg command that produces one distorted sample file.
    fn build_vmaf_trial_encode(
        &self,
        request: &VmafTrialEncodeRequest,
    ) -> Result<FfmpegEncodeCommand>;
}

/// Serializable encode metadata stored in `transcode_outputs.encode_metadata_json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EncodeMetadata {
    /// Stable encoder backend identifier.
    pub encoder_kind: String,
    /// FFmpeg version string captured during encode, when available.
    pub ffmpeg_version: Option<String>,
    /// Encoded output duration in microseconds.
    pub duration_us: u64,
    /// Per-sample, per-CRF VMAF measurements used for CRF selection.
    pub vmaf_samples: Vec<SampleMeasurement>,
    /// CRF chosen for the full encode, absent for passthrough outputs.
    pub chosen_crf: Option<u8>,
    /// Mean VMAF from a final encoded-output spot check, when performed.
    pub spot_check_vmaf: Option<f32>,
    /// HLS `CODECS` attribute value for this output.
    pub codecs: String,
    /// Encoded output resolution as `(width, height)`.
    pub resolution: (u32, u32),
    /// Estimated or measured HLS `BANDWIDTH` attribute value.
    pub bandwidth: Option<u64>,
    /// HLS video range advertised for this output.
    pub video_range: VideoRange,
    /// Color downgrade relationship recorded for this output, when any.
    pub color_downgrade: Option<ColorDowngrade>,
}

/// HLS video range advertised by an encoded output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum VideoRange {
    /// Standard dynamic range output.
    Sdr,
    /// HDR PQ output.
    Pq,
    /// HDR HLG output.
    Hlg,
}

/// Durable color downgrade kind associated with an encoded output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColorDowngrade {
    /// Dolby Vision source downgraded to HDR10.
    DvToHdr10,
    /// HDR10 source downgraded to SDR.
    Hdr10ToSdr,
    /// Dolby Vision source downgraded to SDR.
    DvToSdr,
}

/// Select evenly distributed source samples as `(start, duration)` pairs.
pub fn select_samples(
    source_duration: Duration,
    sample_count: usize,
    sample_seconds: u32,
) -> Vec<(Duration, Duration)> {
    if sample_count == 0 || sample_seconds == 0 {
        return Vec::new();
    }

    let sample_duration = Duration::from_secs(u64::from(sample_seconds));
    if source_duration < sample_duration {
        return Vec::new();
    }

    let source_us = source_duration.as_micros();
    let sample_us = sample_duration.as_micros();
    let mut samples = Vec::with_capacity(sample_count);
    let mut previous_start = None;
    for index in 0..sample_count {
        let start_us = source_us.saturating_mul((index + 1) as u128) / ((sample_count + 1) as u128);
        if start_us.saturating_add(sample_us) > source_us || previous_start == Some(start_us) {
            continue;
        }
        samples.push((duration_from_micros(start_us), sample_duration));
        previous_start = Some(start_us);
    }

    samples
}

/// Encode each selected sample at each trial CRF, measure VMAF, and aggregate results.
pub async fn measure_sample_crfs<E>(
    source: &SourceContext,
    encoder: &E,
    runner: &PipelineRunner,
    config: &VmafSamplingConfig,
) -> Result<Vec<SampleMeasurement>>
where
    E: VmafSampleEncoder,
{
    let backend = FfmpegVmafTrialBackend { encoder, runner };
    measure_sample_crfs_with_backend(source, config, &backend).await
}

/// Fit `vmaf = a*crf + b` per sample, average coefficients, solve target CRF, and clamp.
pub fn fit_crf(measurements: &[SampleMeasurement], target_vmaf: f32, crf_clamp: (u8, u8)) -> u8 {
    let min_crf = crf_clamp.0.min(crf_clamp.1);
    let max_crf = crf_clamp.0.max(crf_clamp.1);
    let mut by_sample: BTreeMap<usize, Vec<&SampleMeasurement>> = BTreeMap::new();
    for measurement in measurements {
        if measurement.mean_vmaf.is_finite() {
            by_sample
                .entry(measurement.sample_idx)
                .or_default()
                .push(measurement);
        }
    }

    let mut coefficient_count = 0u32;
    let mut slope_sum = 0.0f32;
    let mut intercept_sum = 0.0f32;
    for sample_measurements in by_sample.values() {
        if let Some((slope, intercept)) = fit_sample(sample_measurements) {
            slope_sum += slope;
            intercept_sum += intercept;
            coefficient_count += 1;
        }
    }

    if coefficient_count == 0 {
        return min_crf;
    }

    let slope = slope_sum / coefficient_count as f32;
    let intercept = intercept_sum / coefficient_count as f32;
    if slope.abs() <= f32::EPSILON || !slope.is_finite() || !intercept.is_finite() {
        return min_crf;
    }

    let crf = ((target_vmaf - intercept) / slope).round();
    if !crf.is_finite() {
        return min_crf;
    }

    crf.clamp(f32::from(min_crf), f32::from(max_crf)) as u8
}

#[derive(Debug, Clone, PartialEq)]
struct VmafTrialRequest {
    sample_idx: usize,
    crf: u8,
    reference: InputSpec,
    output_path: PathBuf,
    vmaf_log_path: PathBuf,
    ffmpeg_binary: PathBuf,
    video: VideoOutputSpec,
    filters: Vec<VideoFilter>,
}

trait VmafTrialBackend {
    fn measure_trial<'a>(&'a self, request: VmafTrialRequest) -> TranscodeFuture<'a, f32>;
}

struct FfmpegVmafTrialBackend<'a, E> {
    encoder: &'a E,
    runner: &'a PipelineRunner,
}

impl<E> VmafTrialBackend for FfmpegVmafTrialBackend<'_, E>
where
    E: VmafSampleEncoder,
{
    fn measure_trial<'a>(&'a self, request: VmafTrialRequest) -> TranscodeFuture<'a, f32> {
        Box::pin(async move {
            if let Some(parent) = request
                .output_path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
            {
                tokio::fs::create_dir_all(parent).await?;
            }
            match tokio::fs::remove_file(&request.output_path).await {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => return Err(Error::Io(err)),
            }

            let encode_request = VmafTrialEncodeRequest {
                sample_idx: request.sample_idx,
                crf: request.crf,
                input: request.reference.clone(),
                output_path: request.output_path.clone(),
                video: request.video,
                filters: request.filters,
            };
            let command = self.encoder.build_vmaf_trial_encode(&encode_request)?;
            let (cancel_tx, cancel_rx) = oneshot::channel();
            let result = self.runner.run(command, cancel_rx).await;
            drop(cancel_tx);
            result?;

            FfmpegVmafCommand::new(
                request.ffmpeg_binary,
                request.reference,
                InputSpec::file(request.output_path),
            )
            .log_path(request.vmaf_log_path)
            .measure(self.runner)
            .await
        })
    }
}

async fn measure_sample_crfs_with_backend(
    source: &SourceContext,
    config: &VmafSamplingConfig,
    backend: &dyn VmafTrialBackend,
) -> Result<Vec<SampleMeasurement>> {
    let duration = source
        .probe
        .duration
        .ok_or_else(|| Error::VmafFailed("source duration is missing".to_owned()))?;
    let samples = select_samples(duration, config.sample_count, config.sample_seconds);
    tokio::fs::create_dir_all(&config.work_dir).await?;

    let mut measurements =
        Vec::with_capacity(samples.len().saturating_mul(config.trial_crfs.len()));
    for (sample_idx, (start, duration)) in samples.into_iter().enumerate() {
        for crf in &config.trial_crfs {
            let mut reference = InputSpec::file(source.probe.source_path.clone());
            reference.start_us = Some(duration_to_micros(start)?);
            reference.duration_us = Some(duration_to_micros(duration)?);

            let mut video = config.video.clone();
            video.crf = Some(*crf);
            let request = VmafTrialRequest {
                sample_idx,
                crf: *crf,
                reference,
                output_path: config
                    .work_dir
                    .join(format!("sample-{sample_idx}-crf-{crf}.mkv")),
                vmaf_log_path: config
                    .work_dir
                    .join(format!("sample-{sample_idx}-crf-{crf}.vmaf.json")),
                ffmpeg_binary: config.ffmpeg_binary.clone(),
                video,
                filters: config.filters.clone(),
            };
            let mean_vmaf = backend.measure_trial(request).await?;
            measurements.push(SampleMeasurement {
                sample_idx,
                crf: *crf,
                mean_vmaf,
            });
        }
    }

    Ok(measurements)
}

fn fit_sample(measurements: &[&SampleMeasurement]) -> Option<(f32, f32)> {
    if measurements.len() < 2 {
        return None;
    }

    let count = measurements.len() as f32;
    let sum_x = measurements
        .iter()
        .map(|measurement| f32::from(measurement.crf))
        .sum::<f32>();
    let sum_y = measurements
        .iter()
        .map(|measurement| measurement.mean_vmaf)
        .sum::<f32>();
    let sum_xy = measurements
        .iter()
        .map(|measurement| f32::from(measurement.crf) * measurement.mean_vmaf)
        .sum::<f32>();
    let sum_x2 = measurements
        .iter()
        .map(|measurement| f32::from(measurement.crf).powi(2))
        .sum::<f32>();
    let denominator = count.mul_add(sum_x2, -(sum_x * sum_x));
    if denominator.abs() <= f32::EPSILON {
        return None;
    }

    let slope = (count.mul_add(sum_xy, -(sum_x * sum_y))) / denominator;
    let intercept = (sum_y - slope * sum_x) / count;
    (slope.is_finite() && intercept.is_finite()).then_some((slope, intercept))
}

fn duration_to_micros(duration: Duration) -> Result<u64> {
    u64::try_from(duration.as_micros())
        .map_err(|_| Error::VmafFailed("duration exceeds microsecond range".to_owned()))
}

fn duration_from_micros(micros: u128) -> Duration {
    let capped = micros.min(u128::from(u64::MAX));
    Duration::from_micros(capped as u64)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use kino_core::{Id, ProbeResult};

    use super::*;
    use crate::{ColorOutput, Preset, VideoCodec};

    struct FakeBackend;

    impl VmafTrialBackend for FakeBackend {
        fn measure_trial<'a>(&'a self, request: VmafTrialRequest) -> TranscodeFuture<'a, f32> {
            Box::pin(async move { Ok(120.0 - f32::from(request.crf) + request.sample_idx as f32) })
        }
    }

    #[test]
    fn select_samples_uses_even_offsets() {
        let samples = select_samples(Duration::from_secs(100), 3, 12);

        assert_eq!(
            samples,
            vec![
                (Duration::from_secs(25), Duration::from_secs(12)),
                (Duration::from_secs(50), Duration::from_secs(12)),
                (Duration::from_secs(75), Duration::from_secs(12)),
            ]
        );
    }

    #[test]
    fn select_samples_returns_fewer_when_samples_would_overrun() {
        let samples = select_samples(Duration::from_secs(20), 3, 12);

        assert_eq!(
            samples,
            vec![(Duration::from_secs(5), Duration::from_secs(12))]
        );
    }

    #[test]
    fn select_samples_rejects_empty_requests() {
        assert!(select_samples(Duration::from_secs(100), 0, 12).is_empty());
        assert!(select_samples(Duration::from_secs(100), 3, 0).is_empty());
        assert!(select_samples(Duration::from_secs(10), 3, 12).is_empty());
    }

    #[test]
    fn fit_crf_solves_average_linear_curve() {
        let measurements = measurements_for_fit();

        assert_eq!(fit_crf(&measurements, 92.0, (18, 30)), 24);
    }

    #[test]
    fn fit_crf_clamps_extremes() {
        let measurements = measurements_for_fit();

        assert_eq!(fit_crf(&measurements, 120.0, (18, 30)), 18);
        assert_eq!(fit_crf(&measurements, 50.0, (18, 30)), 30);
    }

    #[tokio::test]
    async fn measure_sample_crfs_aggregates_backend_results() -> Result<()> {
        let source = SourceContext {
            source_file_id: Id::new(),
            probe: ProbeResult {
                source_path: PathBuf::from("/library/movie/source.mkv"),
                container: None,
                title: None,
                duration: Some(Duration::from_secs(100)),
                video_streams: Vec::new(),
                audio_streams: Vec::new(),
                subtitle_streams: Vec::new(),
            },
        };
        let config = sampling_config();

        let measurements = measure_sample_crfs_with_backend(&source, &config, &FakeBackend).await?;

        assert_eq!(
            measurements,
            vec![
                SampleMeasurement {
                    sample_idx: 0,
                    crf: 18,
                    mean_vmaf: 102.0,
                },
                SampleMeasurement {
                    sample_idx: 0,
                    crf: 24,
                    mean_vmaf: 96.0,
                },
                SampleMeasurement {
                    sample_idx: 1,
                    crf: 18,
                    mean_vmaf: 103.0,
                },
                SampleMeasurement {
                    sample_idx: 1,
                    crf: 24,
                    mean_vmaf: 97.0,
                },
            ]
        );

        Ok(())
    }

    #[test]
    fn encode_metadata_round_trips_json() -> std::result::Result<(), serde_json::Error> {
        let metadata = EncodeMetadata {
            encoder_kind: "software".to_owned(),
            ffmpeg_version: Some("ffmpeg version 7.1".to_owned()),
            duration_us: 7_200_000_000,
            vmaf_samples: vec![SampleMeasurement {
                sample_idx: 0,
                crf: 24,
                mean_vmaf: 95.4,
            }],
            chosen_crf: Some(24),
            spot_check_vmaf: Some(95.1),
            codecs: "hvc1.1.6.L120.90,mp4a.40.2".to_owned(),
            resolution: (1920, 1080),
            bandwidth: Some(5_000_000),
            video_range: VideoRange::Pq,
            color_downgrade: Some(ColorDowngrade::DvToHdr10),
        };

        let json = serde_json::to_string(&metadata)?;
        let decoded: EncodeMetadata = serde_json::from_str(&json)?;

        assert_eq!(decoded, metadata);
        Ok(())
    }

    fn measurements_for_fit() -> Vec<SampleMeasurement> {
        [0usize, 1]
            .into_iter()
            .flat_map(|sample_idx| {
                [18u8, 24, 30]
                    .into_iter()
                    .map(move |crf| SampleMeasurement {
                        sample_idx,
                        crf,
                        mean_vmaf: (-2.0_f32).mul_add(f32::from(crf), 140.0),
                    })
            })
            .collect()
    }

    fn sampling_config() -> VmafSamplingConfig {
        VmafSamplingConfig {
            sample_count: 2,
            sample_seconds: 12,
            trial_crfs: vec![18, 24],
            work_dir: PathBuf::from("/tmp/kino-vmaf-test"),
            ffmpeg_binary: PathBuf::from("ffmpeg"),
            video: VideoOutputSpec {
                codec: VideoCodec::Hevc,
                crf: None,
                preset: Preset::Medium,
                bit_depth: 10,
                color: ColorOutput::SdrBt709,
                max_resolution: Some((1920, 1080)),
            },
            filters: Vec::new(),
        }
    }
}
