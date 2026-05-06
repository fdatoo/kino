//! Shared types for the Kino workspace: configuration, ids, timestamps.
//!
//! Other crates depend on this one for the canonical [`Id`], [`Timestamp`],
//! and [`Config`] types and the per-crate `thiserror` error convention
//! documented in `AGENTS.md`.

pub mod config;
pub mod id;
pub mod time;
pub mod tracing;

pub use config::Config;
pub use id::Id;
pub use time::Timestamp;
