//! Stream variant selection for playback routing.

use kino_core::VariantKind;
use kino_library::CatalogMediaItem;

/// Client playback capability hints used to choose a stream variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityHint {
    /// Video codecs the client can play directly.
    pub codecs: Vec<String>,
    /// Containers the client can demux directly.
    pub container_supports: Vec<String>,
    /// Maximum video height the client wants, when known.
    pub max_height: Option<u32>,
    /// Whether the client can display HDR content, when known.
    pub hdr: Option<bool>,
}

/// Variant routing decision for a catalog media item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VariantDecision {
    /// A playable variant was selected.
    Match {
        /// Selected catalog variant id.
        variant_id: String,
    },
    /// No existing variant can satisfy the request.
    NoSuitableVariant {
        /// Structured reason suitable for Phase 3 on-the-fly variant building.
        reason: String,
    },
}

/// Select a stream variant for a catalog item and client capabilities.
///
/// Phase 2 prefers the first source variant whose codec is listed in
/// `hint.codecs`, then falls back to the first source variant unconditionally.
/// The fallback is intentional: Phase 3 will replace codec/container/resolution
/// mismatches with `NoSuitableVariant` so the on-the-fly variant builder can
/// create a compatible output.
pub fn select(item: &CatalogMediaItem, hint: &CapabilityHint) -> VariantDecision {
    let first_source = item
        .variants
        .iter()
        .find(|variant| variant.kind == VariantKind::Source);

    let selected = if hint.codecs.is_empty() {
        first_source
    } else {
        item.variants
            .iter()
            .find(|variant| {
                variant.kind == VariantKind::Source
                    && hint.codecs.contains(&variant.capabilities.codec)
            })
            .or(first_source)
    };

    match selected {
        Some(variant) => VariantDecision::Match {
            variant_id: variant.variant_id.clone(),
        },
        None => VariantDecision::NoSuitableVariant {
            reason: "no source variant available".to_owned(),
        },
    }
}

#[cfg(test)]
mod tests {
    use kino_core::{
        CatalogStreamVariant, Id, MediaItemKind, Timestamp, VariantCapabilities, VariantKind,
    };
    use kino_library::{CatalogArtwork, CatalogMediaItem};

    use super::{CapabilityHint, VariantDecision, select};

    #[test]
    fn generic_hint_matches_first_source_variant() {
        let item = media_item(vec![source_variant("source-file-id", "h264")]);
        let hint = CapabilityHint {
            codecs: Vec::new(),
            container_supports: Vec::new(),
            max_height: None,
            hdr: None,
        };

        let decision = select(&item, &hint);

        assert_eq!(
            decision,
            VariantDecision::Match {
                variant_id: "source-file-id".to_owned()
            }
        );
    }

    #[test]
    fn codec_hint_matches_source_variant() {
        let item = media_item(vec![source_variant("source-file-id", "hevc")]);
        let hint = CapabilityHint {
            codecs: vec!["hevc".to_owned()],
            container_supports: Vec::new(),
            max_height: None,
            hdr: None,
        };

        let decision = select(&item, &hint);

        assert_eq!(
            decision,
            VariantDecision::Match {
                variant_id: "source-file-id".to_owned()
            }
        );
    }

    #[test]
    fn missing_source_variant_returns_structured_reason() {
        let item = media_item(Vec::new());
        let hint = CapabilityHint {
            codecs: Vec::new(),
            container_supports: Vec::new(),
            max_height: None,
            hdr: None,
        };

        let decision = select(&item, &hint);

        assert_eq!(
            decision,
            VariantDecision::NoSuitableVariant {
                reason: "no source variant available".to_owned()
            }
        );
    }

    #[test]
    fn phase_two_accepts_source_when_codec_hint_does_not_match() {
        let item = media_item(vec![source_variant("source-file-id", "h264")]);
        let hint = CapabilityHint {
            codecs: vec!["hevc".to_owned()],
            container_supports: Vec::new(),
            max_height: None,
            hdr: None,
        };

        let decision = select(&item, &hint);

        assert_eq!(
            decision,
            VariantDecision::Match {
                variant_id: "source-file-id".to_owned()
            }
        );
    }

    fn media_item(variants: Vec<CatalogStreamVariant>) -> CatalogMediaItem {
        let now = Timestamp::now();
        CatalogMediaItem {
            id: Id::new(),
            media_kind: MediaItemKind::Movie,
            canonical_identity_id: None,
            season_number: None,
            episode_number: None,
            title: None,
            description: None,
            release_date: None,
            year: None,
            cast: Vec::new(),
            artwork: CatalogArtwork::default(),
            variants,
            source_files: Vec::new(),
            subtitle_tracks: Vec::new(),
            created_at: now,
            updated_at: now,
        }
    }

    fn source_variant(variant_id: &str, codec: &str) -> CatalogStreamVariant {
        CatalogStreamVariant {
            variant_id: variant_id.to_owned(),
            kind: VariantKind::Source,
            capabilities: VariantCapabilities {
                codec: codec.to_owned(),
                container: "mkv".to_owned(),
                resolution: Some("1080p".to_owned()),
                hdr: None,
            },
            stream_url: format!("/api/v1/stream/sourcefile/{variant_id}/file.mkv"),
        }
    }
}
