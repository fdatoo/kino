//! Runtime encoder backend detection.

use std::{collections::BTreeSet, path::PathBuf};

use tracing::{debug, info};

use super::{EncoderKind, EncoderRegistry, SoftwareEncoder};
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

    for kind in [
        EncoderKind::Qsv,
        EncoderKind::Vaapi,
        EncoderKind::VideoToolbox,
    ] {
        if config.allow.contains(&kind) {
            debug!(
                "hardware backend not yet implemented: kind={}",
                kind.as_str()
            );
        }
    }

    Ok(registry)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn default_detection_registers_software_encoder() -> crate::Result<()> {
        let registry = available_encoders(&DetectionConfig::default()).await?;

        assert_eq!(registry.encoders().len(), 1);
        assert_eq!(registry.encoders()[0].kind(), EncoderKind::Software);

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
