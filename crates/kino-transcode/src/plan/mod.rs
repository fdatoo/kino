//! Output planning policies and canonical transcode profiles.

pub mod policy;
pub mod profile;
pub mod variant;
pub mod vmaf;

pub use policy::{DefaultPolicy, DefaultPolicyConfig, OutputPolicy, SourceContext};
pub use profile::TranscodeProfile;
pub use variant::{AudioPolicyKind, ColorTarget, Container, PlannedVariant, VariantKind};
pub use vmaf::{
    ColorDowngrade, EncodeMetadata, SampleMeasurement, VideoRange, VmafSampleEncoder,
    VmafSamplingConfig, VmafTrialEncodeRequest, fit_crf, measure_sample_crfs, select_samples,
};
