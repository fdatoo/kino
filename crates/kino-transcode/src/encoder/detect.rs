//! Runtime encoder backend detection.

use std::{collections::BTreeSet, path::PathBuf};

#[cfg(target_os = "macos")]
use std::path::Path;

#[cfg(target_os = "macos")]
use kino_core::Id;
use tracing::{debug, info};

#[cfg(target_os = "macos")]
use tokio::process::Command;

use super::{EncoderKind, EncoderRegistry, SoftwareEncoder, VideoToolboxEncoder};
use crate::Result;

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
/// acceleration and trial encodes in their backend PRs.
pub async fn available_encoders(config: &DetectionConfig) -> Result<EncoderRegistry> {
    let mut registry = EncoderRegistry::new();

    if config.allow.contains(&EncoderKind::Software) {
        registry.register(Box::new(SoftwareEncoder::with_binary(
            config.ffmpeg_binary.clone(),
        )));
        info!("encoder backend available: kind=software");
    }

    for kind in [EncoderKind::Qsv, EncoderKind::Vaapi] {
        if config.allow.contains(&kind) {
            debug!(
                "hardware backend not yet implemented: kind={}",
                kind.as_str()
            );
        }
    }

    if config.allow.contains(&EncoderKind::VideoToolbox)
        && videotoolbox_available(&config.ffmpeg_binary).await
    {
        registry.register(Box::new(VideoToolboxEncoder::with_binary(
            config.ffmpeg_binary.clone(),
        )));
        info!("encoder backend available: kind=videotoolbox");
    }

    Ok(registry)
}

#[cfg(target_os = "macos")]
async fn videotoolbox_available(ffmpeg_binary: &Path) -> bool {
    let output = match Command::new(ffmpeg_binary)
        .args(["-hide_banner", "-hwaccels"])
        .output()
        .await
    {
        Ok(output) => output,
        Err(err) => {
            debug!(error = %err, "videotoolbox detection failed to run ffmpeg hwaccel probe");
            return false;
        }
    };

    if !output.status.success() {
        debug!(
            status = output.status.code().unwrap_or(-1),
            stderr = %String::from_utf8_lossy(&output.stderr),
            "videotoolbox detection ffmpeg hwaccel probe failed"
        );
        return false;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    if !stdout
        .lines()
        .map(str::trim)
        .any(|line| line == "videotoolbox")
    {
        debug!("videotoolbox hwaccel not reported by ffmpeg");
        return false;
    }

    trial_encode_videotoolbox(ffmpeg_binary).await
}

#[cfg(not(target_os = "macos"))]
async fn videotoolbox_available(_ffmpeg_binary: &PathBuf) -> bool {
    false
}

#[cfg(target_os = "macos")]
async fn trial_encode_videotoolbox(ffmpeg_binary: &Path) -> bool {
    let source_path =
        std::env::temp_dir().join(format!("kino-videotoolbox-probe-{}.mp4", Id::new()));
    let source_output = match Command::new(ffmpeg_binary)
        .args([
            "-y",
            "-hide_banner",
            "-nostdin",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=640x360:rate=1",
            "-frames:v",
            "1",
            "-map",
            "0:v:0",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
        ])
        .arg(&source_path)
        .output()
        .await
    {
        Ok(output) => output,
        Err(err) => {
            debug!(error = %err, "videotoolbox detection failed to create trial input");
            cleanup_videotoolbox_probe(&source_path).await;
            return false;
        }
    };

    if !source_output.status.success() {
        debug!(
            status = source_output.status.code().unwrap_or(-1),
            stderr = %String::from_utf8_lossy(&source_output.stderr),
            "videotoolbox detection trial input creation failed"
        );
        cleanup_videotoolbox_probe(&source_path).await;
        return false;
    }

    let output = match Command::new(ffmpeg_binary)
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
        .arg(&source_path)
        .args([
            "-frames:v",
            "1",
            "-map",
            "0:v:0",
            "-c:v",
            "hevc_videotoolbox",
            "-allow_sw",
            "0",
            "-f",
            "null",
            "-",
        ])
        .output()
        .await
    {
        Ok(output) => output,
        Err(err) => {
            debug!(error = %err, "videotoolbox detection failed to run trial encode");
            cleanup_videotoolbox_probe(&source_path).await;
            return false;
        }
    };

    if !output.status.success() {
        debug!(
            status = output.status.code().unwrap_or(-1),
            stderr = %String::from_utf8_lossy(&output.stderr),
            "videotoolbox detection trial encode failed"
        );
        cleanup_videotoolbox_probe(&source_path).await;
        return false;
    }

    cleanup_videotoolbox_probe(&source_path).await;
    true
}

#[cfg(target_os = "macos")]
async fn cleanup_videotoolbox_probe(path: &Path) {
    match tokio::fs::remove_file(path).await {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            debug!(error = %err, path = %path.display(), "videotoolbox detection failed to remove trial input");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn default_detection_registers_software_encoder() -> crate::Result<()> {
        let registry = available_encoders(&DetectionConfig::default()).await?;

        assert!(
            registry
                .encoders()
                .iter()
                .any(|encoder| encoder.kind() == EncoderKind::Software)
        );

        Ok(())
    }

    #[tokio::test]
    async fn detection_honors_software_allow_list() -> crate::Result<()> {
        let registry = available_encoders(&DetectionConfig {
            ffmpeg_binary: PathBuf::from("ffmpeg"),
            allow: BTreeSet::from([EncoderKind::Qsv]),
        })
        .await?;

        assert!(registry.encoders().is_empty());

        Ok(())
    }
}
