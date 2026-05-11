//! Shared types for the Kino workspace: configuration, ids, timestamps.
//!
//! Other crates depend on this one for the canonical [`Id`], [`Timestamp`],
//! and [`Config`] types and the per-crate `thiserror` error convention
//! documented in `AGENTS.md`.

pub mod catalog;
pub mod config;
pub mod id;
pub mod identity;
pub mod request;
pub mod time;
pub mod tracing;
pub mod user;

pub use catalog::{MediaItem, MediaItemKind, SourceFile, TranscodeOutput};
pub use config::{CanonicalLayoutTransfer, Config, LibraryConfig};
pub use id::Id;
pub use identity::{
    CanonicalIdentity, CanonicalIdentityId, CanonicalIdentityKind, CanonicalIdentityProvider,
    CanonicalIdentitySource, TmdbId,
};
pub use request::{Request, RequestFailureReason, RequestRequester, RequestState, RequestTarget};
pub use time::Timestamp;
pub use user::{SEEDED_USER_ID, User};
