# kino-core scaffolding — Design Spec

**Linear:** F-187 (sub-issues: F-197 errors, F-198 config, F-199 time + ids)
**Phase:** 0 — Foundations
**Date:** 2026-05-05

## Done when

- Other crates can `use kino_core::{Config, Id, Timestamp}` and don't need their own equivalents.
- A reference `kino.toml.example` is committed at the repo root.
- Workspace lints enforce the no-`unwrap`/no-`expect` rule for non-test code.
- CLAUDE.md documents the error convention, the docstring/comment convention, and the configuration surface (env var names, nesting rule).
- `cargo build --workspace`, `cargo test --workspace`, `cargo fmt --all -- --check`, and `cargo clippy --workspace --all-targets -- -D warnings` all pass.

The binary loading `Config` is **out of scope** for this issue; that's a follow-up.

---

## 1. Crate surface

`kino-core` exports three types at the crate root:

```rust
pub use config::Config;
pub use id::Id;
pub use time::Timestamp;

pub mod config;  // Config + ConfigError
pub mod id;      // Id + ParseIdError
pub mod time;    // Timestamp + ParseTimestampError
```

No shared `Error` enum is exported. Each crate defines its own (see §2).

---

## 2. Error convention

Documented in CLAUDE.md; no shared error code in `kino-core`.

- Every library crate defines its own `Error` enum with `thiserror`. Variant `#[error("...")]` messages start lowercase and have no trailing period.
- Errors at crate boundaries are **the** boundary — internal helpers may return concrete error types and convert via `#[from]` at the public surface.
- No `anyhow` in any library crate. `anyhow::Error` is allowed only in the top-level `kino` binary, where `main` may return `anyhow::Result<()>`.
- No `unwrap`/`expect` in non-test code; enforced by `clippy::unwrap_used` and `clippy::expect_used` workspace lints (see §5). Tests can `unwrap` freely — clippy already exempts `#[cfg(test)]` modules.
- `kino-core` itself has exactly one error type, `config::ConfigError`. It is **not** re-exported at the crate root.

`Id` and `Timestamp` constructors are infallible. Their `FromStr` impls return crate-local parse-error types kept in their modules: `id::ParseIdError`, `time::ParseTimestampError`.

---

## 3. `Id` and `Timestamp`

### `kino_core::id`

```rust
//! UUID v7 identifiers used for every persisted entity.

use std::{fmt, str::FromStr};
use serde::{Deserialize, Serialize};

/// A UUID v7 identifier, used for every entity persisted by Kino.
///
/// Ids are time-prefixed, so they sort lexicographically by creation time.
/// Construct new ids with [`Id::new`]; round-trip persisted ids through
/// [`Id::from_uuid`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Id(uuid::Uuid);

impl Id {
    /// Generate a fresh UUID v7 id from the current time.
    pub fn new() -> Self {
        Self(uuid::Uuid::now_v7())
    }

    /// Wrap an existing UUID without checking its version.
    ///
    /// Use this on the persistence read path where the value has already been
    /// validated. New ids should be created with [`Id::new`].
    pub fn from_uuid(uuid: uuid::Uuid) -> Self {
        Self(uuid)
    }

    /// The wrapped UUID.
    pub fn as_uuid(&self) -> uuid::Uuid {
        self.0
    }
}

impl fmt::Display for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0.hyphenated(), f)
    }
}

impl fmt::Debug for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Id({})", self.0.hyphenated())
    }
}

impl FromStr for Id {
    type Err = ParseIdError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(uuid::Uuid::parse_str(s)?))
    }
}

/// Returned when a string is not a valid UUID.
#[derive(Debug, thiserror::Error)]
#[error("invalid id: {0}")]
pub struct ParseIdError(#[from] uuid::Error);
```

Notes:

- `FromStr` accepts any valid UUID, not only v7. Permissive on read, strict on write — `Id::new` is the only path that mints a v7. Rejecting non-v7 on parse would break round-tripping older fixtures and offers no real safety win for an internal type.
- No `Default` impl: `Id::new()` is explicit; "default id" has no meaningful value.
- `serde(transparent)` makes the wire form a bare UUID string.

### `kino_core::time`

```rust
//! UTC timestamps used across the workspace.

use std::{fmt, str::FromStr};
use serde::{Deserialize, Serialize};
use time::{OffsetDateTime, UtcOffset, format_description::well_known::Rfc3339};

/// A UTC timestamp.
///
/// The UTC invariant is enforced at construction — once you have a
/// `Timestamp`, the offset is always UTC. Wire format is RFC3339.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp(OffsetDateTime);

impl Timestamp {
    /// The current UTC time.
    pub fn now() -> Self {
        Self(OffsetDateTime::now_utc())
    }

    /// Convert any `OffsetDateTime` to a UTC `Timestamp`.
    pub fn from_offset(t: OffsetDateTime) -> Self {
        Self(t.to_offset(UtcOffset::UTC))
    }

    /// The wrapped `OffsetDateTime` (always UTC).
    pub fn as_offset(&self) -> OffsetDateTime {
        self.0
    }
}

impl fmt::Display for Timestamp { /* RFC3339 via time::format_description::well_known::Rfc3339 */ }
impl fmt::Debug   for Timestamp { /* delegates to Display */ }

impl FromStr for Timestamp {
    type Err = ParseTimestampError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let t = OffsetDateTime::parse(s, &Rfc3339)?;
        Ok(Self::from_offset(t))
    }
}

/// Returned when a string is not a valid RFC3339 timestamp.
#[derive(Debug, thiserror::Error)]
#[error("invalid timestamp: {0}")]
pub struct ParseTimestampError(#[from] time::error::Parse);

// Serde via the time crate's RFC3339 codec, not derive(Serialize/Deserialize),
// because we need to normalize to UTC on deserialize.
impl Serialize for Timestamp { /* delegates to time::serde::rfc3339 */ }
impl<'de> Deserialize<'de> for Timestamp {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let t = time::serde::rfc3339::deserialize(d)?;
        Ok(Self::from_offset(t))
    }
}
```

Notes:

- `from_offset` normalizes to UTC, so the invariant holds regardless of input offset.
- Manual `Serialize`/`Deserialize` (rather than derive + `serde(transparent)`) so deserialization always normalizes to UTC.

---

## 4. `Config` + loader

### Types

```rust
//! Configuration: a TOML file plus environment-variable overrides.

use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
};
use serde::Deserialize;

/// Kino's startup configuration.
///
/// Loaded from a TOML file (path resolved by [`Config::load`]) with
/// environment-variable overrides under the `KINO_` prefix. See the workspace
/// `kino.toml.example` for a full reference.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Path to the SQLite database file. Required.
    pub database_path: PathBuf,

    /// Root directory of the on-disk media library. Required.
    pub library_root: PathBuf,

    /// HTTP/gRPC server settings. Optional; defaults documented on
    /// [`ServerConfig`].
    #[serde(default)]
    pub server: ServerConfig,

    /// Logging filter. Accepts any tracing-subscriber `EnvFilter` expression.
    /// Defaults to `"info"`.
    #[serde(default = "default_log_level")]
    pub log_level: String,
}

/// HTTP/gRPC server settings.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    /// Address the server binds to. Defaults to `127.0.0.1:7777`.
    #[serde(default = "default_listen")]
    pub listen: SocketAddr,
}

fn default_listen() -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 7777)
}

fn default_log_level() -> String {
    "info".into()
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self { listen: default_listen() }
    }
}
```

`deny_unknown_fields` is intentional: typos in user TOML become hard errors instead of silent defaults.

### Loader

```rust
impl Config {
    /// Load configuration from disk and the environment.
    ///
    /// Resolves the file path in this order:
    /// 1. The `KINO_CONFIG` environment variable, if set.
    /// 2. `./kino.toml` in the current working directory.
    ///
    /// Then layers `KINO_`-prefixed environment variables on top, with `__`
    /// separating nested sections (e.g. `KINO_SERVER__LISTEN`).
    ///
    /// Returns [`ConfigError`] on missing file, parse errors, missing required
    /// fields, or invalid values.
    pub fn load() -> Result<Self, ConfigError> { /* figment merge + extract */ }

    /// Load configuration from an explicit file path (skips `KINO_CONFIG`).
    /// Useful for tests and the CLI's `--config` flag.
    pub fn load_from(path: impl AsRef<Path>) -> Result<Self, ConfigError> { /* … */ }
}
```

Implementation sketch:

```rust
use figment::{Figment, providers::{Format, Toml, Env}};

// load(): file is optional unless KINO_CONFIG is set.
let explicit = std::env::var_os("KINO_CONFIG").map(PathBuf::from);
let path = explicit.clone().unwrap_or_else(|| PathBuf::from("kino.toml"));

let mut fig = Figment::new();
if explicit.is_some() || path.exists() {
    // Pre-check: figment's Toml::file is soft on missing, but if the user
    // pointed KINO_CONFIG at a path or the default kino.toml is present,
    // unreadable file should be an Io error, not a field-missing error.
    std::fs::metadata(&path).map_err(|e| ConfigError::Io { path: path.clone(), source: e })?;
    fig = fig.merge(Toml::file(&path));
}
fig.merge(Env::prefixed("KINO_").split("__").ignore(&["CONFIG"]))
    .extract::<Config>()
    .map_err(ConfigError::from)
```

`load_from(path)` is the same shape but always pre-checks `path` and never reads `KINO_CONFIG`. `KINO_CONFIG` selects the file path and is not a `Config` field; `Env::ignore(&["CONFIG"])` keeps `deny_unknown_fields` from rejecting it.

### `ConfigError`

```rust
/// Errors produced while loading [`Config`].
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The config file could not be read.
    #[error("reading config file {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// The config file or environment overrides failed to parse or validate.
    ///
    /// Includes missing required fields, unknown fields, and type mismatches.
    /// The wrapped figment error carries source location.
    #[error("invalid config: {0}")]
    Invalid(#[from] figment::Error),
}
```

When `Io` fires:

- `load_from(path)` returns `Io` if `path` does not exist or is not readable. Caller asked for that specific file; missing it is always a hard error.
- `load()` returns `Io` only when `KINO_CONFIG` is set and points to an unreadable path. If `KINO_CONFIG` is unset and `./kino.toml` is absent, the loader proceeds with env-only sources — missing required fields then surface as `Invalid`, which gives a clearer message than "file not found" when the user is configuring entirely through env vars.

Both `load` and `load_from` pre-check the path with `std::fs::metadata` before merging, because figment's `Toml::file` is soft on missing files (it would otherwise surface as a `MissingField` error during extract).

### `kino.toml.example` (committed at repo root)

```toml
# Path to the SQLite database file. Required.
database_path = "/var/lib/kino/kino.db"

# Root directory of the on-disk media library. Required.
library_root = "/srv/media"

# Logging filter. Optional. Defaults to "info".
# Accepts any tracing-subscriber EnvFilter expression.
log_level = "info"

[server]
# Address the server binds to. Optional. Defaults to 127.0.0.1:7777.
listen = "127.0.0.1:7777"
```

### Documented env vars

- `KINO_CONFIG` — path to the TOML file (overrides default `./kino.toml`).
- `KINO_DATABASE_PATH`, `KINO_LIBRARY_ROOT`, `KINO_LOG_LEVEL` — top-level fields.
- `KINO_SERVER__LISTEN` — nested field; double underscore separates segments.

---

## 5. Workspace changes — deps, lints, CLAUDE.md

### Workspace `Cargo.toml`

```toml
[workspace.dependencies]
# (existing)
tokio              = { version = "1", features = ["full"] }
serde              = { version = "1", features = ["derive"] }
tracing            = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
# (new)
thiserror = "2"
uuid      = { version = "1", features = ["v7", "serde"] }
time      = { version = "0.3", features = ["serde-well-known", "macros"] }
figment   = { version = "0.10", features = ["toml", "env"] }

[workspace.lints.rust]
unsafe_code    = "deny"
unused_imports = "warn"

[workspace.lints.clippy]
all         = "warn"
unwrap_used = "warn"
expect_used = "warn"
```

`-D warnings` in CI promotes the new clippy warnings to fatal.

### `crates/kino-core/Cargo.toml`

```toml
[package]
name = "kino-core"
version.workspace = true
edition.workspace = true
license.workspace = true

[lints]
workspace = true

[dependencies]
serde     = { workspace = true }
thiserror = { workspace = true }
uuid      = { workspace = true }
time      = { workspace = true }
figment   = { workspace = true }
```

### CLAUDE.md edits

Three additions under "Code conventions":

1. **Errors** — codifies per-crate `thiserror` enums, no `anyhow` in libraries, no `unwrap`/`expect` outside tests, lint-enforced. Replaces the existing "Errors" bullet.
2. **Docstrings & comments** (new subsection) — every public item in a library crate gets a `///` doc comment; every library crate gets a `//!` crate-level doc; inline `//` comments stay rare and explain *why*.
3. **Configuration** (new subsection) — points at `kino_core::Config`, lists the documented env var names, documents the `__` nesting rule.

The existing **Time + ids** bullet is updated to reference the now-real `kino_core::Id` and `kino_core::Timestamp` (replacing "live in `kino-core` once added").

### Binary

`crates/kino/src/main.rs` is unchanged. Wiring `Config::load()` into the binary is a separate follow-up issue.

---

## 6. Testing

All tests colocated in `#[cfg(test)] mod tests` per CLAUDE.md.

### `id`

- `Id::new()` produces a UUID v7 (`get_version_num() == 7`).
- Two ids minted in sequence sort `<=` (the v7 time-prefix invariant; not strictly `<` because of same-millisecond collisions).
- Round-trip: `Id → Display → FromStr → Id` is identity.
- Serde round-trip: `serde_json::to_string` then `from_str` is identity, and the JSON form is a bare quoted UUID string.
- `FromStr` rejects non-UUID strings with `ParseIdError`.

### `time`

- `Timestamp::now()` is UTC.
- `Timestamp::from_offset` of a non-UTC `OffsetDateTime` normalizes to UTC and preserves the instant.
- RFC3339 round-trip via `Display`/`FromStr` is identity for UTC values.
- Serde round-trip emits RFC3339 with `Z` suffix.
- Deserializing a non-UTC RFC3339 string normalizes to UTC.

### `config`

- Happy path: a temp-dir TOML with all required fields parses.
- Defaults: omitted `[server]` and `log_level` use the documented defaults.
- Missing required field (`database_path` absent, no env override) → `ConfigError::Invalid` whose message names the field.
- `deny_unknown_fields`: a typo (`databse_path`) → `ConfigError::Invalid`.
- Env override: `KINO_DATABASE_PATH` set, omitted from TOML → loads successfully with the env value.
- Nested env override: `KINO_SERVER__LISTEN=0.0.0.0:8080` → `config.server.listen` reflects it.
- `KINO_CONFIG` resolution: pointing it at a temp file loads that file rather than `./kino.toml`.
- `KINO_CONFIG` set to a nonexistent path → `ConfigError::Io` (not `Invalid`).
- `load_from(nonexistent)` → `ConfigError::Io`.
- `load()` with no `KINO_CONFIG`, no `./kino.toml`, and env vars supplying every required field → `Ok` (file is optional in this case).

Env-var tests use `figment::Jail` to scope env mutations — built for this case, no `serial_test` or `unsafe { std::env::set_var }` needed. The loader takes its env from figment for the same reason.
