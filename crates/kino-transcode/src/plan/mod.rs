//! Output planning policies and canonical transcode profiles.

pub mod policy;
pub mod profile;
pub mod variant;

pub use policy::{DefaultPolicy, DefaultPolicyConfig, OutputPolicy, SourceContext};
pub use profile::TranscodeProfile;
pub use variant::{AudioPolicyKind, ColorTarget, Container, PlannedVariant, VariantKind};
