#![cfg(feature = "hwaccel-tests")]

use std::path::Path;
use std::time::Duration;

#[cfg(target_os = "macos")]
use kino_transcode::VideoToolboxEncoder;
use kino_transcode::{
    AudioPolicy, ColorOutput, HlsOutputSpec, Preset, QsvEncoder, VaapiEncoder, VideoCodec,
    VideoOutputSpec, encoder::SoftwareEncodeContext,
};
use tokio::process::Command;

const FFMPEG: &str = "ffmpeg";
const RENDER_NODE: &str = "/dev/dri/renderD128";

#[tokio::test]
async fn qsv_encoder_runs_tiny_hls_encode() -> Result<(), Box<dyn std::error::Error>> {
    if !qsv_available().await? {
        return Ok(());
    }

    let temp = tempfile::tempdir()?;
    let input = temp.path().join("input.mp4");
    generate_input(&input).await?;

    let output_dir = temp.path().join("qsv");
    tokio::fs::create_dir_all(&output_dir).await?;

    let encoder = QsvEncoder::with_binary(FFMPEG);
    let command = encoder.build_command(&SoftwareEncodeContext {
        input_path: input,
        video: VideoOutputSpec {
            codec: VideoCodec::H264,
            crf: Some(24),
            preset: Preset::Medium,
            bit_depth: 8,
            color: ColorOutput::SdrBt709,
            max_resolution: Some((64, 64)),
        },
        audio: AudioPolicy::None,
        filters: Vec::new(),
        hls: hls_output(&output_dir),
    });

    run_encode(command.into_command()).await?;
    assert_hls_segments(&output_dir).await
}

#[tokio::test]
async fn vaapi_encoder_runs_tiny_hls_encode() -> Result<(), Box<dyn std::error::Error>> {
    if !vaapi_available().await? {
        return Ok(());
    }

    let temp = tempfile::tempdir()?;
    let input = temp.path().join("input.mp4");
    generate_input(&input).await?;

    let output_dir = temp.path().join("vaapi");
    tokio::fs::create_dir_all(&output_dir).await?;

    let encoder = VaapiEncoder::with_binary(FFMPEG, RENDER_NODE);
    let command = encoder.build_command(&SoftwareEncodeContext {
        input_path: input,
        video: VideoOutputSpec {
            codec: VideoCodec::H264,
            crf: Some(24),
            preset: Preset::Medium,
            bit_depth: 8,
            color: ColorOutput::SdrBt709,
            max_resolution: Some((64, 64)),
        },
        audio: AudioPolicy::None,
        filters: Vec::new(),
        hls: hls_output(&output_dir),
    });

    run_encode(command.into_command()).await?;
    assert_hls_segments(&output_dir).await
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn videotoolbox_encoder_runs_tiny_hls_encode() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let input = temp.path().join("input.mp4");
    generate_input(&input).await?;

    if !videotoolbox_available(&input).await? {
        return Ok(());
    }

    let output_dir = temp.path().join("videotoolbox");
    tokio::fs::create_dir_all(&output_dir).await?;

    let encoder = VideoToolboxEncoder::with_binary(FFMPEG);
    let command = encoder.build_command(&SoftwareEncodeContext {
        input_path: input,
        video: VideoOutputSpec {
            codec: VideoCodec::H264,
            crf: Some(24),
            preset: Preset::Medium,
            bit_depth: 8,
            color: ColorOutput::SdrBt709,
            max_resolution: Some((64, 64)),
        },
        audio: AudioPolicy::None,
        filters: Vec::new(),
        hls: hls_output(&output_dir),
    });

    run_encode(command.into_command()).await?;
    assert_hls_segments(&output_dir).await
}

async fn qsv_available() -> Result<bool, Box<dyn std::error::Error>> {
    let hwaccels = match Command::new(FFMPEG)
        .args(["-hide_banner", "-hwaccels"])
        .output()
        .await
    {
        Ok(output) => output,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(Box::new(err)),
    };
    if !hwaccels.status.success()
        || !String::from_utf8_lossy(&hwaccels.stdout)
            .lines()
            .any(|line| line.trim().eq_ignore_ascii_case("qsv"))
    {
        return Ok(false);
    }

    let output = Command::new(FFMPEG)
        .args([
            "-hide_banner",
            "-nostdin",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=c=black:size=64x64:rate=1:duration=1",
            "-frames:v",
            "1",
            "-an",
            "-c:v",
            "h264_qsv",
            "-f",
            "null",
            "-",
        ])
        .output()
        .await?;

    Ok(output.status.success())
}

async fn vaapi_available() -> Result<bool, Box<dyn std::error::Error>> {
    if tokio::fs::File::open(RENDER_NODE).await.is_err() {
        return Ok(false);
    }

    let device = format!("vaapi=hw:{RENDER_NODE}");
    let output = Command::new(FFMPEG)
        .args([
            "-hide_banner",
            "-nostdin",
            "-loglevel",
            "error",
            "-init_hw_device",
            &device,
            "-filter_hw_device",
            "hw",
            "-f",
            "lavfi",
            "-i",
            "color=c=black:size=64x64:rate=1:duration=1",
            "-vf",
            "format=nv12,hwupload",
            "-frames:v",
            "1",
            "-an",
            "-c:v",
            "h264_vaapi",
            "-f",
            "null",
            "-",
        ])
        .output()
        .await?;

    Ok(output.status.success())
}

#[cfg(target_os = "macos")]
async fn videotoolbox_available(input: &Path) -> Result<bool, Box<dyn std::error::Error>> {
    let hwaccels = match Command::new(FFMPEG)
        .args(["-hide_banner", "-hwaccels"])
        .output()
        .await
    {
        Ok(output) => output,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(Box::new(err)),
    };
    if !hwaccels.status.success()
        || !String::from_utf8_lossy(&hwaccels.stdout)
            .lines()
            .any(|line| line.trim().eq_ignore_ascii_case("videotoolbox"))
    {
        return Ok(false);
    }

    let output = Command::new(FFMPEG)
        .args([
            "-hide_banner",
            "-nostdin",
            "-loglevel",
            "error",
            "-hwaccel",
            "videotoolbox",
            "-hwaccel_output_format",
            "videotoolbox_vld",
            "-i",
        ])
        .arg(input)
        .args([
            "-frames:v",
            "1",
            "-an",
            "-c:v",
            "h264_videotoolbox",
            "-vf",
            "hwdownload,format=nv12",
            "-pix_fmt",
            "nv12",
            "-f",
            "null",
            "-",
        ])
        .output()
        .await?;

    Ok(output.status.success())
}

async fn generate_input(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let output = Command::new(FFMPEG)
        .args([
            "-hide_banner",
            "-nostdin",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=c=black:size=64x64:rate=1:duration=1",
            "-frames:v",
            "1",
            "-an",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
        ])
        .arg(path)
        .output()
        .await?;

    assert!(
        output.status.success(),
        "synthetic input generation failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

async fn run_encode(mut command: Command) -> Result<(), Box<dyn std::error::Error>> {
    let output = command.output().await?;
    assert!(
        output.status.success(),
        "hardware encode failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

fn hls_output(output_dir: &Path) -> HlsOutputSpec {
    HlsOutputSpec::cmaf_vod(output_dir, Duration::from_secs(1))
}

async fn assert_hls_segments(output_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    assert!(
        tokio::fs::metadata(output_dir.join("media.m3u8"))
            .await
            .is_ok()
    );
    assert!(
        tokio::fs::metadata(output_dir.join("init.mp4"))
            .await
            .is_ok()
    );
    assert!(
        tokio::fs::metadata(output_dir.join("seg-00000.m4s"))
            .await
            .is_ok()
    );
    Ok(())
}
