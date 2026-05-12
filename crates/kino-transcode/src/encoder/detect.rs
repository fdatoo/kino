//! Runtime encoder backend detection.

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use tokio::process::Command;
use tracing::{debug, info};

use super::{
    EncoderKind, EncoderRegistry, QsvEncoder, SoftwareEncoder, VaapiEncoder, VideoToolboxEncoder,
};
use crate::Result;

const VAAPI_RENDER_NODE: &str = "/dev/dri/renderD128";

/// Host encoder detection inputs.
pub struct DetectionConfig {
    /// FFmpeg binary used by detected encoder backends.
    pub ffmpeg_binary: PathBuf,
    /// Encoder backend families allowed to participate in detection.
    pub allow: BTreeSet<EncoderKind>,
}

impl Default for DetectionConfig {
    fn default() -> Self {
        Self {
            ffmpeg_binary: PathBuf::from("ffmpeg"),
            allow: BTreeSet::from([
                EncoderKind::Software,
                EncoderKind::Qsv,
                EncoderKind::Vaapi,
                EncoderKind::VideoToolbox,
            ]),
        }
    }
}

/// Probe the host for available encoder backends.
///
/// Returns the live registry of usable encoders. Software is always present
/// when allowed. Hardware backends are detected by probing FFmpeg hardware
/// acceleration and trial-encoding a tiny synthetic input.
pub async fn available_encoders(config: &DetectionConfig) -> Result<EncoderRegistry> {
    let mut registry = EncoderRegistry::new();

    if config.allow.contains(&EncoderKind::Software) {
        registry.register(Box::new(SoftwareEncoder::with_binary(
            config.ffmpeg_binary.clone(),
        )));
        info!("encoder backend available: kind=software");
    }

    if config.allow.contains(&EncoderKind::Qsv) {
        match probe_qsv(&config.ffmpeg_binary).await {
            Ok(()) => {
                registry.register(Box::new(QsvEncoder::with_binary(
                    config.ffmpeg_binary.clone(),
                )));
                info!("encoder backend available: kind=qsv");
            }
            Err(reason) => {
                debug!(reason, "encoder backend unavailable: kind=qsv");
            }
        }
    }

    if config.allow.contains(&EncoderKind::Vaapi) {
        let render_node = PathBuf::from(VAAPI_RENDER_NODE);
        match probe_vaapi(&config.ffmpeg_binary, &render_node).await {
            Ok(()) => {
                registry.register(Box::new(VaapiEncoder::with_binary(
                    config.ffmpeg_binary.clone(),
                    render_node,
                )));
                info!("encoder backend available: kind=vaapi");
            }
            Err(reason) => {
                debug!(reason, "encoder backend unavailable: kind=vaapi");
            }
        }
    }

    if config.allow.contains(&EncoderKind::VideoToolbox) {
        match probe_videotoolbox(&config.ffmpeg_binary).await {
            Ok(()) => {
                registry.register(Box::new(VideoToolboxEncoder::with_binary(
                    config.ffmpeg_binary.clone(),
                )));
                info!("encoder backend available: kind=videotoolbox");
            }
            Err(reason) => {
                debug!(reason, "encoder backend unavailable: kind=videotoolbox");
            }
        }
    }

    Ok(registry)
}

#[cfg(target_os = "macos")]
async fn probe_videotoolbox(binary: &Path) -> std::result::Result<(), String> {
    let hwaccels = ffmpeg_hwaccels(binary).await?;
    if hwaccels
        .lines()
        .any(|line| line.trim().eq_ignore_ascii_case("videotoolbox"))
    {
        Ok(())
    } else {
        Err("ffmpeg hwaccels output did not include videotoolbox".to_owned())
    }
}

#[cfg(not(target_os = "macos"))]
async fn probe_videotoolbox(_binary: &Path) -> std::result::Result<(), String> {
    Err("videotoolbox is only available on macOS".to_owned())
}

async fn probe_qsv(binary: &Path) -> std::result::Result<(), String> {
    let hwaccels = ffmpeg_hwaccels(binary).await?;
    if !hwaccels
        .lines()
        .any(|line| line.trim().eq_ignore_ascii_case("qsv"))
    {
        return Err("ffmpeg hwaccels output did not include qsv".to_owned());
    }

    trial_encode(
        binary,
        &[
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
        ],
    )
    .await
}

async fn probe_vaapi(binary: &Path, render_node: &Path) -> std::result::Result<(), String> {
    tokio::fs::metadata(render_node)
        .await
        .map_err(|err| format!("render node {} unavailable: {err}", render_node.display()))?;

    let device = format!("vaapi=hw:{}", render_node.display());
    trial_encode(
        binary,
        &[
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
        ],
    )
    .await
}

async fn ffmpeg_hwaccels(binary: &Path) -> std::result::Result<String, String> {
    let output = Command::new(binary)
        .args(["-hide_banner", "-hwaccels"])
        .output()
        .await
        .map_err(|err| format!("ffmpeg hwaccels probe failed to start: {err}"))?;

    if !output.status.success() {
        return Err(format!(
            "ffmpeg hwaccels probe exited with status {}: {}",
            status_code(&output.status),
            stderr_tail(&output.stderr)
        ));
    }

    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

async fn trial_encode(binary: &Path, args: &[&str]) -> std::result::Result<(), String> {
    let output = Command::new(binary)
        .args(args)
        .output()
        .await
        .map_err(|err| format!("trial encode failed to start: {err}"))?;

    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "trial encode exited with status {}: {}",
            status_code(&output.status),
            stderr_tail(&output.stderr)
        ))
    }
}

fn status_code(status: &std::process::ExitStatus) -> i32 {
    status.code().unwrap_or(-1)
}

fn stderr_tail(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let mut lines = text.lines().rev().take(8).collect::<Vec<_>>();
    lines.reverse();
    if lines.is_empty() {
        String::new()
    } else {
        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn default_detection_registers_software_encoder() -> crate::Result<()> {
        let registry = available_encoders(&DetectionConfig {
            ffmpeg_binary: PathBuf::from("ffmpeg"),
            allow: BTreeSet::from([EncoderKind::Software]),
        })
        .await?;

        assert_eq!(registry.encoders().len(), 1);
        assert_eq!(registry.encoders()[0].kind(), EncoderKind::Software);

        Ok(())
    }

    #[tokio::test]
    async fn detection_honors_software_allow_list() -> crate::Result<()> {
        let registry = available_encoders(&DetectionConfig {
            ffmpeg_binary: PathBuf::from("/definitely/not/ffmpeg"),
            allow: BTreeSet::from([EncoderKind::Qsv]),
        })
        .await?;

        assert!(registry.encoders().is_empty());

        Ok(())
    }
}
