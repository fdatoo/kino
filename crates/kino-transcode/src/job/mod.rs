//! Durable transcode job lifecycle types.

pub mod state;

pub use state::{JobState, try_transition};
