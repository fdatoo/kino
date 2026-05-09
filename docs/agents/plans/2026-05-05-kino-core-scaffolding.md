# kino-core Scaffolding Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement `kino-core::{Config, Id, Timestamp}` plus the per-crate `thiserror` convention, so other crates can depend on these without reinventing them.

**Architecture:** Three modules in `kino-core` (`config`, `id`, `time`). UUID v7 newtype, UTC `OffsetDateTime` newtype, and a figment-based TOML+env config loader with `KINO_` prefix and `__` nesting. No shared `Error` enum — each crate owns its own. Workspace clippy lints (`unwrap_used`, `expect_used`) enforce error discipline.

**Tech Stack:** Rust 2024, `thiserror`, `uuid` (v7), `time` (RFC3339, UTC), `figment` (TOML + env merging), `serde`, `serde_json` and `tempfile` for tests.

**Spec:** `docs/agents/specs/2026-05-05-kino-core-scaffolding.md`

**Linear:** F-187 (sub-issues F-197, F-198, F-199)

**Branch:** `fdatoo/f-187-kino-core-scaffolding`

**Commit convention reminder:** This repo's CLAUDE.md/AGENTS.md is strict: **one-line semantic commits, no body, no `Co-Authored-By:` trailers, no agent footers, ever.** Allowed prefixes: `feat`, `fix`, `chore`, `refactor`, `test`, `docs`, `perf`, `build`. Scope is the crate name without `kino-` prefix (`core`, `db`) or area (`repo`, `agents`).

**Verification command (run before any "done" claim):**
```bash
cargo build --workspace \
 && cargo test --workspace \
 && cargo fmt --all -- --check \
 && cargo clippy --workspace --all-targets -- -D warnings
```

---

## File structure

| File | Disposition | Responsibility |
|---|---|---|
| `Cargo.toml` | modify | Add workspace deps (`thiserror`, `uuid`, `time`, `figment`, `serde_json`, `tempfile`) and clippy lints (`unwrap_used`, `expect_used`). |
| `crates/kino-core/Cargo.toml` | modify | Declare runtime deps + dev-deps from the workspace set. |
| `crates/kino-core/src/lib.rs` | rewrite | Crate-level docs, module declarations, re-exports. |
| `crates/kino-core/src/id.rs` | create | `Id` newtype, `ParseIdError`. |
| `crates/kino-core/src/time.rs` | create | `Timestamp` newtype, `ParseTimestampError`. |
| `crates/kino-core/src/config.rs` | create | `Config`, `ServerConfig`, `ConfigError`, `load`/`load_from`. |
| `kino.toml.example` | create | Reference config file at repo root. |
| `AGENTS.md` | modify | Add Errors, Docstrings & comments, Configuration subsections; update Time + ids bullet. |

`CLAUDE.md` is a symlink to `AGENTS.md` — editing `AGENTS.md` updates both.

---

## Task 1: Workspace dependencies and lints

**Goal:** Make new deps and stricter clippy lints available before any code depends on them.

**Files:**
- Modify: `Cargo.toml` (workspace root)
- Modify: `crates/kino-core/Cargo.toml`

- [ ] **Step 1: Add new workspace dependencies and clippy lints**

Replace the `[workspace.dependencies]` and `[workspace.lints.clippy]` sections of `Cargo.toml`. The full file becomes:

```toml
[workspace]
members = [
    "crates/kino-core",
    "crates/kino-db",
    "crates/kino-fulfillment",
    "crates/kino-library",
    "crates/kino-transcode",
    "crates/kino-server",
    "crates/kino-admin",
    "crates/kino-cli",
    "crates/kino",
]
resolver = "2"

[workspace.package]
edition = "2024"
version = "0.1.0"
license = "Apache-2.0"

[workspace.dependencies]
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
thiserror = "2"
uuid = { version = "1", features = ["v7", "serde"] }
time = { version = "0.3", features = ["serde-well-known", "macros"] }
figment = { version = "0.10", features = ["toml", "env"] }
serde_json = "1"
tempfile = "3"

[workspace.lints.rust]
unsafe_code = "deny"
unused_imports = "warn"

[workspace.lints.clippy]
all = "warn"
unwrap_used = "warn"
expect_used = "warn"
```

- [ ] **Step 2: Update kino-core's Cargo.toml**

Replace `crates/kino-core/Cargo.toml` with:

```toml
[package]
name = "kino-core"
version.workspace = true
edition.workspace = true
license.workspace = true

[lints]
workspace = true

[dependencies]
serde = { workspace = true }
thiserror = { workspace = true }
uuid = { workspace = true }
time = { workspace = true }
figment = { workspace = true }

[dev-dependencies]
serde_json = { workspace = true }
tempfile = { workspace = true }
```

- [ ] **Step 3: Verify the workspace still builds**

Run: `cargo build --workspace`
Expected: success (the existing empty `Config {}` placeholder still compiles).

- [ ] **Step 4: Verify clippy is clean**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: success. The new `unwrap_used`/`expect_used` lints have nothing to flag yet — the existing code does not use them.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock crates/kino-core/Cargo.toml
git commit -m "chore(repo): add core dependencies and unwrap/expect lints"
```

---

## Task 2: `Id` newtype (TDD)

**Goal:** Implement the `Id` newtype with UUID v7 generation, RFC-style display, parsing, and serde.

**Files:**
- Create: `crates/kino-core/src/id.rs`
- Modify: `crates/kino-core/src/lib.rs`

- [ ] **Step 1: Wire the new module into lib.rs**

Replace `crates/kino-core/src/lib.rs` with:

```rust
//! Shared types for the Kino workspace: configuration, ids, timestamps.
//!
//! Other crates depend on this one for the canonical [`Id`], [`Timestamp`],
//! and [`Config`] types and the per-crate `thiserror` error convention
//! documented in `AGENTS.md`.

pub mod config;
pub mod id;
pub mod time;

pub use config::Config;
pub use id::Id;
pub use time::Timestamp;
```

This will not yet compile because `config`, `id`, and `time` modules don't exist. We will create them across Tasks 2–4. To unblock compilation **only** for this task, temporarily comment out the `config` and `time` lines:

```rust
//! Shared types for the Kino workspace: configuration, ids, timestamps.

// pub mod config;
pub mod id;
// pub mod time;

// pub use config::Config;
pub use id::Id;
// pub use time::Timestamp;
```

The full version is restored at the end of Task 4.

- [ ] **Step 2: Write failing tests for `Id`**

Create `crates/kino-core/src/id.rs` with the test module first (we'll add the impl in Step 4):

```rust
//! UUID v7 identifiers used for every persisted entity.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_produces_uuid_v7() {
        let id = Id::new();
        assert_eq!(id.as_uuid().get_version_num(), 7);
    }

    #[test]
    fn ids_sort_by_creation_time() {
        let a = Id::new();
        // Force a different millisecond so the time-prefix comparison is meaningful.
        std::thread::sleep(std::time::Duration::from_millis(2));
        let b = Id::new();
        assert!(a < b, "expected {a} < {b}");
    }

    #[test]
    fn display_fromstr_roundtrip() {
        let id = Id::new();
        let s = id.to_string();
        let parsed: Id = s.parse().unwrap();
        assert_eq!(id, parsed);
    }

    #[test]
    fn fromstr_rejects_garbage() {
        let err = "not-a-uuid".parse::<Id>().unwrap_err();
        assert!(err.to_string().starts_with("invalid id:"), "got: {err}");
    }

    #[test]
    fn serde_roundtrip_is_bare_string() {
        let id = Id::new();
        let json = serde_json::to_string(&id).unwrap();
        // Bare quoted UUID, no wrapping object.
        assert!(json.starts_with('"') && json.ends_with('"'), "got: {json}");
        let back: Id = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }

    #[test]
    fn from_uuid_preserves_value() {
        let raw = uuid::Uuid::now_v7();
        let id = Id::from_uuid(raw);
        assert_eq!(id.as_uuid(), raw);
    }
}
```

- [ ] **Step 3: Run tests to confirm they fail**

Run: `cargo test -p kino-core --lib id::`
Expected: compile failure ("cannot find type `Id` in this scope" or similar).

- [ ] **Step 4: Implement `Id` and `ParseIdError`**

Append the implementation above the `#[cfg(test)]` block in `crates/kino-core/src/id.rs`:

```rust
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
    /// Use this on the persistence read path where the value has already
    /// been validated. New ids should be created with [`Id::new`].
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

- [ ] **Step 5: Run tests to confirm they pass**

Run: `cargo test -p kino-core --lib id::`
Expected: all 6 tests pass.

- [ ] **Step 6: Verify clippy is clean**

Run: `cargo clippy -p kino-core --all-targets -- -D warnings`
Expected: success. (Tests use `unwrap()`, but clippy's `unwrap_used` lint exempts `#[cfg(test)]` modules by default.)

- [ ] **Step 7: Commit**

```bash
git add crates/kino-core/src/lib.rs crates/kino-core/src/id.rs
git commit -m "feat(core): add Id (UUID v7) newtype"
```

---

## Task 3: `Timestamp` newtype (TDD)

**Goal:** Implement the `Timestamp` newtype as a UTC-only `OffsetDateTime` with RFC3339 serialization that normalizes input offsets.

**Files:**
- Create: `crates/kino-core/src/time.rs`
- Modify: `crates/kino-core/src/lib.rs`

- [ ] **Step 1: Re-enable the `time` module in lib.rs**

Edit `crates/kino-core/src/lib.rs` so the `time` lines are uncommented:

```rust
//! Shared types for the Kino workspace: configuration, ids, timestamps.

// pub mod config;
pub mod id;
pub mod time;

// pub use config::Config;
pub use id::Id;
pub use time::Timestamp;
```

`config` stays commented out; Task 4 restores it.

- [ ] **Step 2: Write failing tests for `Timestamp`**

Create `crates/kino-core/src/time.rs` with imports and the test module:

```rust
//! UTC timestamps used across the workspace.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize, Deserializer, Serializer};
use time::{
    OffsetDateTime, UtcOffset, format_description::well_known::Rfc3339,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_is_utc() {
        let t = Timestamp::now();
        assert_eq!(t.as_offset().offset(), UtcOffset::UTC);
    }

    #[test]
    fn from_offset_normalizes_to_utc() {
        // 2026-05-05T12:00:00+02:00 == 2026-05-05T10:00:00Z
        let plus_two = UtcOffset::from_hms(2, 0, 0).unwrap();
        let dt = OffsetDateTime::now_utc().to_offset(plus_two);
        let ts = Timestamp::from_offset(dt);

        assert_eq!(ts.as_offset().offset(), UtcOffset::UTC);
        assert_eq!(ts.as_offset().unix_timestamp(), dt.unix_timestamp());
    }

    #[test]
    fn display_fromstr_roundtrip() {
        let t = Timestamp::now();
        let s = t.to_string();
        let parsed: Timestamp = s.parse().unwrap();
        assert_eq!(t.as_offset().unix_timestamp_nanos(), parsed.as_offset().unix_timestamp_nanos());
    }

    #[test]
    fn serde_emits_rfc3339_with_z() {
        let t = Timestamp::now();
        let json = serde_json::to_string(&t).unwrap();
        // RFC3339 with Z suffix, wrapped in quotes for JSON.
        assert!(json.ends_with("Z\""), "got: {json}");
        let back: Timestamp = serde_json::from_str(&json).unwrap();
        assert_eq!(t.as_offset().unix_timestamp_nanos(), back.as_offset().unix_timestamp_nanos());
    }

    #[test]
    fn deserialize_normalizes_non_utc_input() {
        let json = "\"2026-05-05T12:00:00+02:00\"";
        let ts: Timestamp = serde_json::from_str(json).unwrap();
        assert_eq!(ts.as_offset().offset(), UtcOffset::UTC);
        // 12:00+02:00 == 10:00Z
        assert_eq!(ts.as_offset().hour(), 10);
    }

    #[test]
    fn fromstr_rejects_garbage() {
        let err = "not-a-timestamp".parse::<Timestamp>().unwrap_err();
        assert!(err.to_string().starts_with("invalid timestamp:"), "got: {err}");
    }
}
```

- [ ] **Step 3: Run tests to confirm they fail**

Run: `cargo test -p kino-core --lib time::`
Expected: compile failure ("cannot find type `Timestamp` in this scope" or similar).

- [ ] **Step 4: Implement `Timestamp` and `ParseTimestampError`**

Add the implementation above the `#[cfg(test)]` block in `crates/kino-core/src/time.rs`:

```rust
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

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = self
            .0
            .format(&Rfc3339)
            .map_err(|_| fmt::Error)?;
        f.write_str(&s)
    }
}

impl fmt::Debug for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

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

impl Serialize for Timestamp {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        time::serde::rfc3339::serialize(&self.0, s)
    }
}

impl<'de> Deserialize<'de> for Timestamp {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let t = time::serde::rfc3339::deserialize(d)?;
        Ok(Self::from_offset(t))
    }
}
```

- [ ] **Step 5: Run tests to confirm they pass**

Run: `cargo test -p kino-core --lib time::`
Expected: all 6 tests pass.

- [ ] **Step 6: Verify clippy is clean**

Run: `cargo clippy -p kino-core --all-targets -- -D warnings`
Expected: success.

- [ ] **Step 7: Commit**

```bash
git add crates/kino-core/src/lib.rs crates/kino-core/src/time.rs
git commit -m "feat(core): add Timestamp (UTC) newtype"
```

---

## Task 4: `Config` types, `ConfigError`, and loader (TDD)

**Goal:** Implement `Config`, `ServerConfig`, `ConfigError`, `Config::load`, and `Config::load_from`. TOML file plus `KINO_`-prefixed env overrides with `__` for nesting; `KINO_CONFIG` selects the file path.

**Files:**
- Create: `crates/kino-core/src/config.rs`
- Modify: `crates/kino-core/src/lib.rs`

- [ ] **Step 1: Re-enable the `config` module in lib.rs**

Restore `crates/kino-core/src/lib.rs` to its final form:

```rust
//! Shared types for the Kino workspace: configuration, ids, timestamps.
//!
//! Other crates depend on this one for the canonical [`Id`], [`Timestamp`],
//! and [`Config`] types and the per-crate `thiserror` error convention
//! documented in `AGENTS.md`.

pub mod config;
pub mod id;
pub mod time;

pub use config::Config;
pub use id::Id;
pub use time::Timestamp;
```

- [ ] **Step 2: Write failing tests for `Config`**

Create `crates/kino-core/src/config.rs` with the imports and test module:

```rust
//! Configuration: a TOML file plus environment-variable overrides.

use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
};

use figment::{
    Figment,
    providers::{Env, Format, Toml},
};
use serde::Deserialize;

#[cfg(test)]
mod tests {
    use super::*;
    use figment::Jail;

    const FULL_TOML: &str = r#"
        database_path = "/var/lib/kino/kino.db"
        library_root = "/srv/media"
        log_level = "debug"

        [server]
        listen = "0.0.0.0:9000"
    "#;

    const REQUIRED_ONLY_TOML: &str = r#"
        database_path = "/db"
        library_root = "/lib"
    "#;

    #[test]
    fn happy_path_full_toml() {
        Jail::expect_with(|jail| {
            jail.create_file("kino.toml", FULL_TOML)?;
            let cfg = Config::load().map_err(|e| e.to_string())?;
            assert_eq!(cfg.database_path, PathBuf::from("/var/lib/kino/kino.db"));
            assert_eq!(cfg.library_root, PathBuf::from("/srv/media"));
            assert_eq!(cfg.log_level, "debug");
            assert_eq!(cfg.server.listen, "0.0.0.0:9000".parse::<SocketAddr>().unwrap());
            Ok(())
        });
    }

    #[test]
    fn defaults_apply_when_optional_fields_omitted() {
        Jail::expect_with(|jail| {
            jail.create_file("kino.toml", REQUIRED_ONLY_TOML)?;
            let cfg = Config::load().map_err(|e| e.to_string())?;
            assert_eq!(cfg.log_level, "info");
            assert_eq!(cfg.server.listen, "127.0.0.1:7777".parse::<SocketAddr>().unwrap());
            Ok(())
        });
    }

    #[test]
    fn missing_required_field_is_invalid_error() {
        Jail::expect_with(|jail| {
            jail.create_file("kino.toml", r#"library_root = "/lib""#)?;
            let err = Config::load().unwrap_err();
            assert!(matches!(err, ConfigError::Invalid(_)), "got: {err:?}");
            assert!(err.to_string().contains("database_path"), "got: {err}");
            Ok(())
        });
    }

    #[test]
    fn deny_unknown_fields_rejects_typos() {
        Jail::expect_with(|jail| {
            jail.create_file(
                "kino.toml",
                r#"
                    databse_path = "/typo"
                    library_root = "/lib"
                "#,
            )?;
            let err = Config::load().unwrap_err();
            assert!(matches!(err, ConfigError::Invalid(_)), "got: {err:?}");
            Ok(())
        });
    }

    #[test]
    fn env_override_supplies_required_field() {
        Jail::expect_with(|jail| {
            jail.create_file("kino.toml", r#"library_root = "/lib""#)?;
            jail.set_env("KINO_DATABASE_PATH", "/from-env");
            let cfg = Config::load().map_err(|e| e.to_string())?;
            assert_eq!(cfg.database_path, PathBuf::from("/from-env"));
            Ok(())
        });
    }

    #[test]
    fn nested_env_override_for_server_listen() {
        Jail::expect_with(|jail| {
            jail.create_file("kino.toml", REQUIRED_ONLY_TOML)?;
            jail.set_env("KINO_SERVER__LISTEN", "0.0.0.0:8080");
            let cfg = Config::load().map_err(|e| e.to_string())?;
            assert_eq!(cfg.server.listen, "0.0.0.0:8080".parse::<SocketAddr>().unwrap());
            Ok(())
        });
    }

    #[test]
    fn kino_config_selects_explicit_file() {
        Jail::expect_with(|jail| {
            jail.create_file("elsewhere.toml", REQUIRED_ONLY_TOML)?;
            // Default ./kino.toml does not exist in the jail.
            jail.set_env("KINO_CONFIG", "elsewhere.toml");
            let cfg = Config::load().map_err(|e| e.to_string())?;
            assert_eq!(cfg.database_path, PathBuf::from("/db"));
            Ok(())
        });
    }

    #[test]
    fn kino_config_pointing_at_missing_file_is_io_error() {
        Jail::expect_with(|jail| {
            jail.set_env("KINO_CONFIG", "does-not-exist.toml");
            let err = Config::load().unwrap_err();
            assert!(matches!(err, ConfigError::Io { .. }), "got: {err:?}");
            Ok(())
        });
    }

    #[test]
    fn load_from_missing_path_is_io_error() {
        let err = Config::load_from("/definitely/not/a/real/path.toml").unwrap_err();
        assert!(matches!(err, ConfigError::Io { .. }), "got: {err:?}");
    }

    #[test]
    fn load_works_with_no_file_when_env_supplies_required_fields() {
        Jail::expect_with(|jail| {
            // No kino.toml, no KINO_CONFIG.
            jail.set_env("KINO_DATABASE_PATH", "/d");
            jail.set_env("KINO_LIBRARY_ROOT", "/l");
            let cfg = Config::load().map_err(|e| e.to_string())?;
            assert_eq!(cfg.database_path, PathBuf::from("/d"));
            assert_eq!(cfg.library_root, PathBuf::from("/l"));
            Ok(())
        });
    }
}
```

- [ ] **Step 3: Run tests to confirm they fail**

Run: `cargo test -p kino-core --lib config::`
Expected: compile failure ("cannot find type `Config` in this scope" or similar).

- [ ] **Step 4: Implement `Config`, `ServerConfig`, `ConfigError`, and the loader**

Add the implementation above the `#[cfg(test)]` block in `crates/kino-core/src/config.rs`:

```rust
/// Kino's startup configuration.
///
/// Loaded from a TOML file (path resolved by [`Config::load`]) with
/// environment-variable overrides under the `KINO_` prefix. See
/// `kino.toml.example` at the repo root for a full reference.
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

/// Errors produced while loading [`Config`].
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The config file could not be read.
    #[error("reading config file {path}: {source}", path = .path.display())]
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

impl Config {
    /// Load configuration from disk and the environment.
    ///
    /// Resolves the file path in this order:
    /// 1. The `KINO_CONFIG` environment variable, if set.
    /// 2. `./kino.toml` in the current working directory (used only if it
    ///    exists; absent file is not an error).
    ///
    /// Then layers `KINO_`-prefixed environment variables on top, with `__`
    /// separating nested sections (e.g. `KINO_SERVER__LISTEN`).
    ///
    /// Returns [`ConfigError::Io`] when `KINO_CONFIG` points at an unreadable
    /// path. Returns [`ConfigError::Invalid`] for parse errors, missing
    /// required fields, unknown fields, or invalid values.
    pub fn load() -> Result<Self, ConfigError> {
        let explicit = std::env::var_os("KINO_CONFIG").map(PathBuf::from);
        let path = explicit
            .clone()
            .unwrap_or_else(|| PathBuf::from("kino.toml"));

        let mut fig = Figment::new();
        if explicit.is_some() || path.exists() {
            std::fs::metadata(&path).map_err(|e| ConfigError::Io {
                path: path.clone(),
                source: e,
            })?;
            fig = fig.merge(Toml::file(&path));
        }
        Ok(fig
            .merge(Env::prefixed("KINO_").split("__").ignore(&["CONFIG"]))
            .extract::<Config>()?)
    }

    /// Load configuration from an explicit file path (skips `KINO_CONFIG`).
    /// Useful for tests and the CLI's `--config` flag.
    pub fn load_from(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        std::fs::metadata(path).map_err(|e| ConfigError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
        Ok(Figment::new()
            .merge(Toml::file(path))
            .merge(Env::prefixed("KINO_").split("__").ignore(&["CONFIG"]))
            .extract::<Config>()?)
    }
}
```

- [ ] **Step 5: Run tests to confirm they pass**

Run: `cargo test -p kino-core --lib config::`
Expected: all 10 tests pass.

If `figment::Jail` is unresolved, the figment crate version may need the `test` feature in dev-dependencies. In that case, change the kino-core `[dev-dependencies]` to add a `figment` line:

```toml
[dev-dependencies]
serde_json = { workspace = true }
tempfile = { workspace = true }
figment = { workspace = true, features = ["test"] }
```

Re-run the tests.

- [ ] **Step 6: Verify the whole crate is green**

Run: `cargo test -p kino-core` and `cargo clippy -p kino-core --all-targets -- -D warnings`
Expected: all tests pass, clippy clean.

- [ ] **Step 7: Commit**

```bash
git add crates/kino-core/src/lib.rs crates/kino-core/src/config.rs crates/kino-core/Cargo.toml Cargo.lock
git commit -m "feat(core): add Config and figment-based loader"
```

---

## Task 5: `kino.toml.example`

**Goal:** Commit a reference config file at the repo root.

**Files:**
- Create: `kino.toml.example`

- [ ] **Step 1: Create the example file**

Create `kino.toml.example` at the repo root with:

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

- [ ] **Step 2: Sanity-check the example parses**

Run a quick ad-hoc check from the repo root:

```bash
cargo test -p kino-core --lib config:: -- --nocapture
```

Expected: all config tests still pass. (No new test wires the example file, but this is a smoke check that nothing in the example contradicts the schema. Optionally, the engineer may add a test that calls `Config::load_from("kino.toml.example")` — not required.)

- [ ] **Step 3: Commit**

```bash
git add kino.toml.example
git commit -m "docs(repo): add kino.toml.example reference config"
```

---

## Task 6: Update AGENTS.md

**Goal:** Codify the error, docstring, and configuration conventions in AGENTS.md (which CLAUDE.md symlinks to).

**Files:**
- Modify: `AGENTS.md`

- [ ] **Step 1: Read the current "Code conventions" section**

Open `AGENTS.md` and locate the "Code conventions" section (search for `## Code conventions`). The current bullets are:

- Rust edition 2024 + workspace lints
- Errors (one-line)
- Logging
- Time + ids (with "live in `kino-core` once added")
- Tests

We are replacing the **Errors** and **Time + ids** bullets and adding two new subsections (**Docstrings & comments** and **Configuration**).

- [ ] **Step 2: Replace the Errors bullet**

Find:

```markdown
- **Errors:** `thiserror` for library errors at crate boundaries. No `anyhow` in
  library code; `anyhow` is acceptable only in the top-level `kino` binary.
```

Replace with:

```markdown
- **Errors:** Each library crate defines its own `Error` enum with `thiserror`.
  Variant `#[error("...")]` messages start lowercase, no trailing period. No
  `anyhow` in library code; it's allowed only in the top-level `kino` binary.
  No `unwrap`/`expect` outside tests — clippy's `unwrap_used` and `expect_used`
  workspace lints enforce this. There is no shared workspace-wide `Error` type;
  errors at crate boundaries are the boundary.
```

- [ ] **Step 3: Replace the "Time + ids" bullet**

Find:

```markdown
- **Time + ids:** UUID v7 for ids, UTC `OffsetDateTime` for timestamps. These types
  live in `kino-core` once added — depend on them, don't redefine them.
```

Replace with:

```markdown
- **Time + ids:** Use `kino_core::Id` (UUID v7) and `kino_core::Timestamp` (UTC).
  Don't redefine them, don't construct ids with `Uuid::new_v4`, don't read clocks
  with `OffsetDateTime::now_utc()` directly — the newtypes own the invariants.
```

- [ ] **Step 4: Add a "Docstrings & comments" subsection**

Insert after the "Time + ids" bullet, before "Tests":

```markdown
- **Docstrings & comments:** Every public item in a library crate gets a `///`
  doc comment — purpose and any non-obvious invariant ("always UTC", "always
  v7"), not the type signature. Every library crate gets a `//!` crate-level
  doc explaining its role. Inline `//` comments stay rare: the default is none,
  and one is added only when *why* is non-obvious (a constraint, a workaround,
  a surprising choice). Don't narrate *what* the code does. Tests and the
  top-level `kino` binary follow a lighter standard.
```

- [ ] **Step 5: Add a "Configuration" subsection**

Insert after "Docstrings & comments", before "Tests":

```markdown
- **Configuration:** `kino_core::Config` is the single config type. The loader
  reads a TOML file (path: `KINO_CONFIG` env var, else `./kino.toml`) and layers
  `KINO_`-prefixed env vars on top. Nesting uses double underscores. Documented
  env vars: `KINO_CONFIG`, `KINO_DATABASE_PATH`, `KINO_LIBRARY_ROOT`,
  `KINO_LOG_LEVEL`, `KINO_SERVER__LISTEN`. The reference TOML lives at
  `kino.toml.example` in the repo root.
```

- [ ] **Step 6: Verify the symlink still resolves**

Run: `cat CLAUDE.md | head -5`
Expected: shows the same first lines as `AGENTS.md` (the symlink resolves).

- [ ] **Step 7: Commit**

```bash
git add AGENTS.md
git commit -m "docs(agents): document error, docstring, and config conventions"
```

---

## Task 7: Final verification

**Goal:** Confirm the whole workspace passes the four CI commands.

- [ ] **Step 1: Build the workspace**

Run: `cargo build --workspace`
Expected: success.

- [ ] **Step 2: Run all tests**

Run: `cargo test --workspace`
Expected: all `kino-core` tests pass (22 total: 6 id, 6 time, 10 config). Other crates have no tests yet.

- [ ] **Step 3: Check formatting**

Run: `cargo fmt --all -- --check`
Expected: no diff.

If formatting is dirty, run `cargo fmt --all` and create a new commit:

```bash
git add -u && git commit -m "chore(repo): cargo fmt"
```

Per CLAUDE.md, prefer new commits over amending — don't amend earlier commits to fix formatting.

- [ ] **Step 4: Run clippy**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: success.

- [ ] **Step 5: Confirm "Done when" criteria from the spec**

- [ ] Other crates can `use kino_core::{Config, Id, Timestamp}` (smoke test: `cargo doc -p kino-core` produces docs for these three exports).
- [ ] `kino.toml.example` exists at the repo root.
- [ ] Workspace `Cargo.toml` includes `clippy::unwrap_used = "warn"` and `clippy::expect_used = "warn"`.
- [ ] `AGENTS.md` has Errors, Docstrings & comments, and Configuration subsections; Time + ids bullet references `kino_core::Id`/`Timestamp`.
- [ ] All four CI commands pass.

- [ ] **Step 6: Push the branch**

```bash
git push -u origin fdatoo/f-187-kino-core-scaffolding
```

(Confirm with the user before pushing if this is the first time the branch is going to the remote.)

---

## Out of scope (intentionally deferred) — Linear coverage

Each deferred item is tracked. Don't do these in this plan; they have homes.

- **Wiring `Config::load()` into `crates/kino/src/main.rs`.** Tracked by **F-195** ("Implement kino binary entry point"). F-195 was previously marked Done alongside F-186 but its acceptance criteria are not met; it has been reopened to Backlog with a comment explaining what's missing. F-195 is blocked on this issue (F-187), F-200 (DB driver), and F-201/F-188 (migrations).

- **Runtime validation of config paths** (e.g. `library_root` exists, `database_path`'s parent is writable). Tracked by **F-291** ("Validate config paths at startup"), filed as a sub-issue of F-187. Deliberately out of scope here so the scaffolding stays focused on types + loader.

Adjacent context (not deferred from F-187 — just intersects):

- **F-204** ("Configure tracing subscriber") plans a `kino_core::tracing::init(&Config)` API. Nothing to do in this plan; just be aware that a future `kino_core::tracing` module will exist alongside the three modules added here.
