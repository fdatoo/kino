//! Encoder backend trait.

use super::{Capabilities, EncoderKind, LaneId, VideoCodec};

// Planned extensions: add supports(&PlannedVariant) and
// build_command(&EncodeContext) once planner and FFmpeg command types exist.
/// Encoder backend interface used by the transcode planner and scheduler.
pub trait Encoder: Send + Sync {
    /// Return this encoder's backend family.
    fn kind(&self) -> EncoderKind;

    /// Return the resource lane this encoder consumes.
    fn lane(&self) -> LaneId;

    /// Return this encoder's static capability declaration.
    fn capabilities(&self) -> &Capabilities;

    /// Whether this encoder can produce an output for the given codec at the
    /// requested resolution and bit depth.
    fn supports_codec(&self, codec: VideoCodec, width: u32, height: u32, bit_depth: u8) -> bool;
}
