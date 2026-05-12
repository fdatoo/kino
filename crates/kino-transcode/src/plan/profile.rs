//! Canonical transcode profile hashes.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use kino_core::Id;

use super::variant::{AudioPolicyKind, ColorTarget, Container, PlannedVariant, VariantKind};
use crate::VideoCodec;

/// Canonical profile identity for deduplicating planned transcode work.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TranscodeProfile {
    /// Source file this profile applies to.
    pub source_file_id: Id,
    /// Stable output role.
    pub kind: VariantKind,
    /// Requested video codec or copy mode.
    pub codec: VideoCodec,
    /// Output container family.
    pub container: Container,
    /// Planned output width, or source width when omitted.
    pub width: Option<u32>,
    /// Planned output video bit depth.
    pub bit_depth: u8,
    /// Planned output color target.
    pub color: ColorTarget,
    /// Planned audio handling policy.
    pub audio: AudioPolicyKind,
    /// Target VMAF quality, omitted for passthrough variants.
    pub vmaf_target: Option<f32>,
}

impl TranscodeProfile {
    /// Construct the canonical profile for a source file and planned variant.
    pub fn from_variant(source_file_id: Id, variant: &PlannedVariant) -> Self {
        Self {
            source_file_id,
            kind: variant.kind,
            codec: variant.codec,
            container: variant.container,
            width: variant.width,
            bit_depth: variant.bit_depth,
            color: variant.color,
            audio: variant.audio,
            vmaf_target: variant.vmaf_target,
        }
    }

    /// Return the SHA-256 digest of [`Self::profile_json`].
    pub fn profile_hash(&self) -> [u8; 32] {
        Sha256::digest(self.profile_json().as_bytes()).into()
    }

    /// Return the canonical JSON string used for profile hashing.
    pub fn profile_json(&self) -> String {
        match serde_json::to_string(self) {
            Ok(json) => json,
            Err(err) => panic!("serializing transcode profile failed: {err}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_json_is_stable_and_hashes_canonical_json() {
        let source_file_id = match "018f16f2-76c0-7c5d-9a38-6dc365f4f062".parse::<Id>() {
            Ok(id) => id,
            Err(err) => panic!("test id should parse: {err}"),
        };
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
        let profile = TranscodeProfile::from_variant(source_file_id, &variant);

        assert_eq!(
            profile.profile_json(),
            concat!(
                r#"{"source_file_id":"018f16f2-76c0-7c5d-9a38-6dc365f4f062","#,
                r#""kind":"high","codec":"hevc","container":"fmp4_cmaf","#,
                r#""width":null,"bit_depth":10,"color":"hdr10","#,
                r#""audio":"stereo_aac_with_surround_passthrough","#,
                r#""vmaf_target":95.0}"#
            )
        );

        let expected_hash: [u8; 32] = Sha256::digest(profile.profile_json().as_bytes()).into();
        assert_eq!(profile.profile_hash(), expected_hash);
    }
}
