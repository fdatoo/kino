#![cfg(all(feature = "hwaccel-tests", target_os = "macos"))]

use std::{collections::BTreeSet, path::Path, time::Duration};

use kino_transcode::encoder::SoftwareEncodeContext;
use kino_transcode::{
    AudioPolicy, ColorOutput, DetectionConfig, EncoderKind, Error, HlsOutputSpec, PipelineRunner,
    Preset, VideoCodec, VideoOutputSpec, VideoToolboxEncoder, verify_outputs,
};
use tokio::{process::Command, sync::oneshot};

#[tokio::test]
async fn videotoolbox_hevc_encode_produces_valid_segments() -> TestResult {
    encode_with_videotoolbox(VideoCodec::Hevc, "hevc-videotoolbox").await
}

#[tokio::test]
async fn videotoolbox_h264_encode_produces_valid_segments() -> TestResult {
    encode_with_videotoolbox(VideoCodec::H264, "h264-videotoolbox").await
}

async fn encode_with_videotoolbox(codec: VideoCodec, output_name: &str) -> TestResult {
    if !videotoolbox_detected().await? {
        return Ok(());
    }

    let temp = tempfile::tempdir()?;
    let source_path = temp.path().join("source.mp4");
    write_source(&source_path).await?;

    let output_dir = temp.path().join(output_name);
    tokio::fs::create_dir_all(&output_dir).await?;

    let encoder = VideoToolboxEncoder::with_binary("ffmpeg");
    let command = encoder.build_command(&SoftwareEncodeContext {
        input_path: source_path,
        video: VideoOutputSpec {
            codec,
            crf: Some(23),
            preset: Preset::Medium,
            bit_depth: 8,
            color: ColorOutput::SdrBt709,
            max_resolution: Some((640, 360)),
        },
        audio: AudioPolicy::None,
        filters: Vec::new(),
        hls: HlsOutputSpec::cmaf_vod(output_dir.clone(), Duration::from_secs(1)),
    })?;

    let (cancel_tx, cancel_rx) = oneshot::channel();
    let result = PipelineRunner::new().run(command, cancel_rx).await;
    drop(cancel_tx);
    if let Err(err) = &result {
        if is_videotoolbox_unavailable(err) {
            return Ok(());
        }
    }
    let _outcome = result?;

    verify_outputs(&output_dir)?;

    Ok(())
}

async fn videotoolbox_detected() -> TestResult<bool> {
    let registry = kino_transcode::available_encoders(&DetectionConfig {
        ffmpeg_binary: "ffmpeg".into(),
        allow: BTreeSet::from([EncoderKind::VideoToolbox]),
    })
    .await?;

    Ok(registry
        .encoders()
        .iter()
        .any(|encoder| encoder.kind() == EncoderKind::VideoToolbox))
}

async fn write_source(path: &Path) -> TestResult {
    let status = Command::new("ffmpeg")
        .args([
            "-y",
            "-hide_banner",
            "-nostdin",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=640x360:rate=24",
            "-t",
            "2",
            "-map",
            "0:v:0",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
        ])
        .arg(path)
        .status()
        .await?;

    if !status.success() {
        return Err(std::io::Error::other(format!(
            "ffmpeg source generation failed with status {}",
            status.code().unwrap_or(-1)
        ))
        .into());
    }

    Ok(())
}

fn is_videotoolbox_unavailable(err: &Error) -> bool {
    match err {
        Error::FfmpegFailed { stderr_tail, .. } => [
            "Cannot create compression session",
            "Failed setup for format videotoolbox_vld",
            "hardware encoder may be busy, or not supported",
            "Impossible to convert between the formats supported by the filter",
        ]
        .iter()
        .any(|marker| stderr_tail.contains(marker)),
        _ => false,
    }
}

type TestResult<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
