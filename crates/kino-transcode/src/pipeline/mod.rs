//! Transcoding pipeline command and runner primitives.

pub mod ffmpeg;
pub mod runner;

pub use ffmpeg::{
    AudioPolicy, ColorOutput, FfmpegEncodeCommand, FfmpegVmafCommand, HlsOutputSpec, InputSpec,
    LogLevel, Preset, VideoFilter, VideoOutputSpec,
};
pub use runner::{PipelineRunner, Progress, RunOutcome, verify_outputs};
