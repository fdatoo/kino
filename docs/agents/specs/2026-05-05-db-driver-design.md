# DB driver — Design Spec

**Linear:** F-200 (parent: F-188 kino-db harness)
**Phase:** 0 — Foundations
**Date:** 2026-05-05

## Done when

- The workspace depends on `sqlx` 0.8 with the SQLite + tokio + macros + migrate features.
- `kino-db` defines its `Error` enum and a smoke test that uses `sqlx::query!` against an in-memory SQLite, exercising the offline-cache path at compile time.
- The repo carries a checked-in `.sqlx/` query cache and a `.cargo/config.toml` that pins `SQLX_OFFLINE=true`; a fresh checkout builds with no env vars set, and IDE rust-analyzer reads the cache without complaint.
- The repo has an ADR system under `docs/adrs/` and ADR-0001 records the sqlx choice.
- CLAUDE.md points at `docs/adrs/` for architecture decisions.
- `cargo build --workspace`, `cargo test --workspace`, `cargo fmt --all -- --check`, and `cargo clippy --workspace --all-targets -- -D warnings` all pass.

The pool wrapper, migration runner, and test fixtures are **out of scope** — they are the rest of F-188.

---

## 1. ADR system

New top-level documentation surface for architecture decisions.

- Directory: `docs/adrs/`.
- File naming: `NNNN-short-slug.md`, four-digit zero-padded, monotonic. ADR-0001 is the first.
- Format: Nygard-lite — sections for `Status`, `Date`, `Context`, `Decision`, `Consequences`. Statuses are `Proposed`, `Accepted`, `Superseded by ADR-NNNN`. No template ceremony beyond that.
- Index: `docs/adrs/README.md` — one-line entry per ADR plus a short paragraph on when to write one (any cross-crate decision someone might later ask "why?" about; an ADR is cheaper than re-litigating the choice in code review).
- CLAUDE.md adds an "ADRs" item under the `Tracking` section pointing at `docs/adrs/`.

ADRs live under `docs/`, not `docs/agents/`, because they are long-lived product/architecture artefacts, not agent working docs.

---

## 2. ADR-0001: SQLite access via sqlx with offline mode

The ADR itself owns the rationale; this section is the summary the spec needs to plan the code changes.

- **Driver:** `sqlx` 0.8.
- **Features:** `runtime-tokio`, `macros`, `sqlite`, `migrate`. `default-features = false` to drop the TLS stack we do not use for SQLite.
- **Query style:** compile-time-checked via `sqlx::query!` / `query_as!`. Runtime `sqlx::query` is allowed for genuinely dynamic SQL but should be the exception.
- **Offline cache:** `.sqlx/` at the workspace root, regenerated with `cargo sqlx prepare --workspace -- --tests` against a local dev DB and committed. Offline mode is forced via `.cargo/config.toml` (`[env] SQLX_OFFLINE = "true"`) so every cargo invocation — IDE rust-analyzer, CI, plain `cargo build` — uses the cache without env tweaks. `cargo sqlx prepare` overrides `SQLX_OFFLINE` to `false` in the env it passes to cargo, so cache regeneration still works.
- **Rejected alternative:** `rusqlite`. Synchronous (would need `spawn_blocking` everywhere off the request path), no compile-time checking, and would require rewriting every query if the Postgres question (vision §8) ever resolves yes.

---

## 3. Workspace + crate changes

### Root `Cargo.toml`

Add to `[workspace.dependencies]`:

```toml
sqlx = { version = "0.8", default-features = false, features = ["runtime-tokio", "macros", "sqlite", "migrate"] }
```

### `crates/kino-db/Cargo.toml`

```toml
[package]
name = "kino-db"
version.workspace = true
edition.workspace = true
license.workspace = true

[lints]
workspace = true

[dependencies]
sqlx      = { workspace = true }
thiserror = { workspace = true }

[dev-dependencies]
tokio = { workspace = true }
```

### `.gitignore`

Add:

```
*.db
*.db-journal
.env
```

### `.env.example` (committed at repo root)

```
# Used by sqlx-cli when regenerating .sqlx/ via `cargo sqlx prepare`.
# Not read at build/test time — those use the checked-in .sqlx/ cache.
DATABASE_URL=sqlite://./dev.db
```

---

## 4. `kino-db` crate body

`crates/kino-db/src/lib.rs`:

```rust
//! SQLite access for Kino.
//!
//! Wraps `sqlx` and (in F-188) exposes the connection-pool, migration runner,
//! and test-fixture helpers used across the workspace. The driver choice and
//! offline-cache workflow are recorded in ADR-0001.

use thiserror::Error;

/// Errors produced by `kino_db`.
#[derive(Debug, Error)]
pub enum Error {
    /// A query, pool, or migration operation failed.
    #[error("database error: {0}")]
    Sqlx(#[from] sqlx::Error),
}

/// Crate-local `Result` alias.
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use sqlx::{Connection, SqliteConnection};

    #[tokio::test]
    async fn smoke_select_one() {
        let mut conn = SqliteConnection::connect("sqlite::memory:").await.unwrap();
        let row = sqlx::query!("SELECT 1 AS one")
            .fetch_one(&mut conn)
            .await
            .unwrap();
        assert_eq!(row.one, 1);
    }
}
```

Notes:

- The `Error` enum exists now (per the per-crate convention in CLAUDE.md) so F-188 can extend rather than introduce it. Wrapping `sqlx::Error` is the only variant for now.
- The smoke test uses `query!` on purpose: it forces the offline cache through the macro at compile time, proving the workflow works end-to-end before F-188 starts adding real queries against a schema.

---

## 5. Offline-cache bootstrap (one-time during this issue)

1. `cargo install sqlx-cli --no-default-features --features sqlite,native-tls`.
2. `export DATABASE_URL=sqlite://./dev.db` (or copy `.env.example` to `.env`).
3. `cargo sqlx database create`.
4. `cargo sqlx prepare --workspace -- --tests` writes `.sqlx/`.
5. Verify: `unset DATABASE_URL; cargo build --workspace && cargo test --workspace` succeeds against the cache.
6. Commit `.sqlx/`.

The dev DB file is gitignored (`*.db`); only `.sqlx/` is committed.

---

## 6. Verification

The four CLAUDE.md commands, with no env vars set:

- `cargo build --workspace`
- `cargo test --workspace`
- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings`

`.cargo/config.toml` pins `SQLX_OFFLINE=true`, so the macro reads from `.sqlx/` regardless of inherited env.

---

## 7. Out of scope (F-188)

- `SqlitePool` wrapper and pool defaults.
- Migration directory layout and `sqlx::migrate!` invocation.
- Test-DB fixture helper that returns a fresh, migrated, in-memory DB.

These land in follow-up sub-issues of F-188 and consume the surface this issue establishes.
