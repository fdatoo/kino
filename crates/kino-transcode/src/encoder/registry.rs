//! Encoder backend registry and selection.

use super::{Encoder, LaneId, VideoCodec};

/// Registered encoder backends available to the transcode scheduler.
pub struct EncoderRegistry {
    encoders: Vec<Box<dyn Encoder>>,
}

impl EncoderRegistry {
    /// Construct an empty encoder registry.
    pub fn new() -> Self {
        Self {
            encoders: Vec::new(),
        }
    }

    /// Construct a registry from an already detected encoder list.
    pub fn from_encoders(encoders: Vec<Box<dyn Encoder>>) -> Self {
        Self { encoders }
    }

    /// Register an encoder backend at the end of the registry preference order.
    pub fn register(&mut self, encoder: Box<dyn Encoder>) {
        self.encoders.push(encoder);
    }

    /// Return registered encoders in insertion order.
    pub fn encoders(&self) -> &[Box<dyn Encoder>] {
        &self.encoders
    }

    /// Return encoders assigned to the requested resource lane.
    pub fn by_lane(&self, lane: LaneId) -> impl Iterator<Item = &dyn Encoder> {
        self.encoders
            .iter()
            .map(Box::as_ref)
            .filter(move |encoder| encoder.lane() == lane)
    }

    /// Select the first encoder that supports the requested codec shape.
    ///
    /// Hardware lanes are considered before CPU software lanes. Within each
    /// lane class, insertion order is preserved so startup detection can encode
    /// local preference.
    pub fn select_for_codec(
        &self,
        codec: VideoCodec,
        width: u32,
        height: u32,
        bit_depth: u8,
    ) -> Option<&dyn Encoder> {
        self.encoders
            .iter()
            .map(Box::as_ref)
            .filter(|encoder| encoder.lane() != LaneId::Cpu)
            .find(|encoder| encoder.supports_codec(codec, width, height, bit_depth))
            .or_else(|| {
                self.encoders
                    .iter()
                    .map(Box::as_ref)
                    .filter(|encoder| encoder.lane() == LaneId::Cpu)
                    .find(|encoder| encoder.supports_codec(codec, width, height, bit_depth))
            })
    }
}

impl Default for EncoderRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoder::{Capabilities, EncoderKind};

    struct FakeEncoder {
        kind: EncoderKind,
        lane: LaneId,
        capabilities: Capabilities,
    }

    impl FakeEncoder {
        fn new(kind: EncoderKind, lane: LaneId, capabilities: Capabilities) -> Self {
            Self {
                kind,
                lane,
                capabilities,
            }
        }
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
    }

    fn fake_encoder(
        kind: EncoderKind,
        lane: LaneId,
        codecs: impl IntoIterator<Item = VideoCodec>,
        max_width: u32,
        max_height: u32,
        ten_bit: bool,
    ) -> Box<dyn Encoder> {
        Box::new(FakeEncoder::new(
            kind,
            lane,
            Capabilities::new(codecs, max_width, max_height, ten_bit, false, false),
        ))
    }

    #[test]
    fn empty_registry_selects_none() {
        let registry = EncoderRegistry::new();

        assert!(
            registry
                .select_for_codec(VideoCodec::Hevc, 1920, 1080, 8)
                .is_none()
        );
    }

    #[test]
    fn single_cpu_encoder_with_hevc_support_is_selected() {
        let registry = EncoderRegistry::from_encoders(vec![fake_encoder(
            EncoderKind::Software,
            LaneId::Cpu,
            [VideoCodec::Hevc],
            3840,
            2160,
            true,
        )]);

        let selected = registry.select_for_codec(VideoCodec::Hevc, 1920, 1080, 10);

        assert_eq!(selected.map(Encoder::lane), Some(LaneId::Cpu));
    }

    #[test]
    fn hardware_encoder_is_selected_before_cpu_encoder() {
        let registry = EncoderRegistry::from_encoders(vec![
            fake_encoder(
                EncoderKind::Software,
                LaneId::Cpu,
                [VideoCodec::Hevc],
                3840,
                2160,
                true,
            ),
            fake_encoder(
                EncoderKind::Qsv,
                LaneId::GpuIntel,
                [VideoCodec::Hevc],
                3840,
                2160,
                true,
            ),
        ]);

        let selected = registry.select_for_codec(VideoCodec::Hevc, 1920, 1080, 10);

        assert_eq!(selected.map(Encoder::lane), Some(LaneId::GpuIntel));
    }

    #[test]
    fn cpu_encoder_is_selected_when_hardware_does_not_support_requested_codec() {
        let registry = EncoderRegistry::from_encoders(vec![
            fake_encoder(
                EncoderKind::Qsv,
                LaneId::GpuIntel,
                [VideoCodec::Hevc],
                3840,
                2160,
                true,
            ),
            fake_encoder(
                EncoderKind::Software,
                LaneId::Cpu,
                [VideoCodec::H264],
                3840,
                2160,
                false,
            ),
        ]);

        let selected = registry.select_for_codec(VideoCodec::H264, 1920, 1080, 8);

        assert_eq!(selected.map(Encoder::lane), Some(LaneId::Cpu));
    }

    #[test]
    fn resolution_above_capability_max_selects_none() {
        let registry = EncoderRegistry::from_encoders(vec![fake_encoder(
            EncoderKind::Software,
            LaneId::Cpu,
            [VideoCodec::Hevc],
            1920,
            1080,
            true,
        )]);

        assert!(
            registry
                .select_for_codec(VideoCodec::Hevc, 3840, 2160, 10)
                .is_none()
        );
    }
}
