//! Encoder backend types and capability declarations.

mod backend;
mod capabilities;
pub mod detect;
mod kind;
pub mod registry;
pub mod software;
mod video;

pub use backend::Encoder;
pub use capabilities::Capabilities;
pub use detect::{DetectionConfig, available_encoders};
pub use kind::{EncoderKind, LaneId};
pub use registry::EncoderRegistry;
pub use software::{SoftwareEncodeContext, SoftwareEncoder};
pub use video::VideoCodec;

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::Error;

    struct FakeEncoder {
        capabilities: Capabilities,
    }

    impl FakeEncoder {
        fn new(capabilities: Capabilities) -> Self {
            Self { capabilities }
        }
    }

    impl Encoder for FakeEncoder {
        fn kind(&self) -> EncoderKind {
            EncoderKind::Software
        }

        fn lane(&self) -> LaneId {
            LaneId::Cpu
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
    }

    #[test]
    fn encoder_kind_round_trips_through_string_id() -> crate::Result<()> {
        for kind in [
            EncoderKind::Software,
            EncoderKind::Qsv,
            EncoderKind::Vaapi,
            EncoderKind::VideoToolbox,
        ] {
            assert_eq!(kind.as_str().parse::<EncoderKind>()?, kind);
        }

        assert!(matches!(
            "unknown".parse::<EncoderKind>(),
            Err(Error::InvalidEncoderKind(value)) if value == "unknown"
        ));

        Ok(())
    }

    #[test]
    fn lane_id_round_trips_through_string_id() -> crate::Result<()> {
        for lane in [LaneId::Cpu, LaneId::GpuIntel, LaneId::GpuVideoToolbox] {
            assert_eq!(lane.as_str().parse::<LaneId>()?, lane);
        }

        assert!(matches!(
            "unknown".parse::<LaneId>(),
            Err(Error::InvalidLaneId(value)) if value == "unknown"
        ));

        Ok(())
    }

    #[test]
    fn video_codec_round_trips_through_string_id() -> crate::Result<()> {
        for codec in [
            VideoCodec::Hevc,
            VideoCodec::H264,
            VideoCodec::Av1,
            VideoCodec::Copy,
        ] {
            assert_eq!(codec.as_str().parse::<VideoCodec>()?, codec);
        }

        assert!(matches!(
            "unknown".parse::<VideoCodec>(),
            Err(Error::InvalidVideoCodec(value)) if value == "unknown"
        ));

        Ok(())
    }

    #[test]
    fn capabilities_accessors_return_configured_values() {
        let capabilities = Capabilities::new(
            [VideoCodec::Hevc, VideoCodec::H264],
            3840,
            2160,
            true,
            true,
            false,
        );

        assert_eq!(
            capabilities.codecs(),
            &BTreeSet::from([VideoCodec::Hevc, VideoCodec::H264])
        );
        assert_eq!(capabilities.max_width(), 3840);
        assert_eq!(capabilities.max_height(), 2160);
        assert!(capabilities.ten_bit());
        assert!(capabilities.hdr10());
        assert!(!capabilities.dv_passthrough());
    }

    #[test]
    fn fake_encoder_exposes_trait_values() {
        let capabilities = Capabilities::new([VideoCodec::Hevc], 3840, 2160, true, true, true);
        let encoder = FakeEncoder::new(capabilities);

        assert_eq!(encoder.kind(), EncoderKind::Software);
        assert_eq!(encoder.lane(), LaneId::Cpu);
        assert_eq!(encoder.capabilities().max_width(), 3840);
        assert!(encoder.supports_codec(VideoCodec::Hevc, 1920, 1080, 10));
        assert!(!encoder.supports_codec(VideoCodec::Av1, 1920, 1080, 10));
        assert!(!encoder.supports_codec(VideoCodec::Hevc, 7680, 4320, 10));
    }
}
