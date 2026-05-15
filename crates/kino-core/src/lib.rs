//! Shared types for the Kino workspace: configuration, ids, timestamps.
//!
//! Other crates depend on this one for the canonical [`Id`], [`Timestamp`],
//! and [`Config`] types and the per-crate `thiserror` error convention
//! documented in `AGENTS.md`.

pub mod catalog;
pub mod config;
pub mod device_token;
pub mod id;
pub mod identity;
pub mod pairing;
pub mod playback_session;
pub mod playback_state;
pub mod probe;
pub mod request;
pub mod time;
pub mod tracing;
pub mod user;

pub use catalog::{
    CatalogStreamVariant, MediaItem, MediaItemKind, SourceFile, TranscodeOutput,
    VariantCapabilities, VariantKind,
};
pub use config::{
    CanonicalLayoutTransfer, Config, EphemeralConfig, LibraryConfig, OcrConfig,
    SessionReaperConfig, TranscodeConfig, TranscodePolicyCompatibilityConfig,
    TranscodePolicyConfig, TranscodePolicyHighConfig, TranscodeSchedulerConfig,
};
pub use device_token::DeviceToken;
pub use id::Id;
pub use identity::{
    CanonicalIdentity, CanonicalIdentityId, CanonicalIdentityKind, CanonicalIdentityProvider,
    CanonicalIdentitySource, TmdbId,
};
pub use pairing::{Pairing, PairingPlatform, PairingStatus};
pub use playback_session::{PlaybackSession, PlaybackSessionStatus};
pub use playback_state::{InvalidPlaybackPosition, PlaybackProgress, Watched, WatchedSource};
pub use probe::{
    DEFAULT_FFPROBE_PROGRAM, DolbyVision, FfprobeFileProbe, MasterDisplay, MaxCll,
    ProbeAudioStream, ProbeContainer, ProbeError, ProbeResult, ProbeSubtitleKind,
    ProbeSubtitleStream, ProbeVideoStream,
};
pub use request::{Request, RequestFailureReason, RequestRequester, RequestState, RequestTarget};
pub use time::Timestamp;
pub use user::{SEEDED_USER_ID, User};
