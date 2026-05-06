# ADR-0001 — SQLite access via sqlx with offline mode

**Status:** Accepted
**Date:** 2026-05-05

## Context

Kino is a single-binary, SQLite-backed system (`kino-vision.md` §4). Every
crate that touches persisted state goes through `kino-db`, so the database
client choice is a cross-cutting decision: it shapes the query style,
async/sync boundary, error surface, and how migrations are shipped.

Two clients realistically fit:

- **`sqlx`** — async, multi-database, with compile-time-checked queries via
  the `query!` / `query_as!` macros. Compile-time checking requires either a
  live `DATABASE_URL` at build time or a checked-in offline query cache
  (`.sqlx/`). Has a built-in migration runner (`sqlx::migrate!`).
- **`rusqlite`** — synchronous, SQLite-only, hand-written queries. Smaller
  dependency surface, no build-time database needed, but every query is
  unchecked SQL inside a Rust string.

Two project facts pulled the decision toward sqlx:

- The runtime is tokio, and the request path is async. `rusqlite` would force
  every database call onto `spawn_blocking`, infecting otherwise-clean async
  code with workarounds.
- `kino-vision.md` §8 lists Postgres support past v1 as an open question.
  sqlx keeps that path open at near-zero cost; rusqlite would mean rewriting
  every query if the answer ever turned out to be yes.

The cost paid for sqlx is the offline-cache workflow: `.sqlx/` must be
regenerated and committed any time a macro query changes. That cost lands on
the developer making the change, not on every other contributor or on CI.

## Decision

Use `sqlx` 0.8 with features `runtime-tokio`, `macros`, `sqlite`, and
`migrate` (and `default-features = false` to drop the TLS stack that SQLite
does not need). Write queries with the compile-time-checked `query!` /
`query_as!` macros; reserve runtime `sqlx::query` for genuinely dynamic SQL.
Commit the `.sqlx/` query cache at the workspace root so fresh checkouts and
CI build with no environment configuration.

## Consequences

- **Offline mode is forced via `.cargo/config.toml`** — `[env] SQLX_OFFLINE = "true"`.
  sqlx's auto-detection (cache present + `DATABASE_URL` unset) works on the
  command line but is unreliable inside IDE rust-analyzer, where the env
  inherited by the language-server's `cargo check` differs from a shell. An
  explicit env var via cargo config avoids the surprise. `cargo sqlx prepare`
  still works because it sets `SQLX_OFFLINE=false` in the process env it
  passes to cargo, which overrides cargo config.
- **Fresh checkouts and CI** need nothing. The `.cargo/config.toml` setting
  means every cargo invocation runs in offline mode by default.
- **Local dev workflow** for changing a macro query:
  1. Install sqlx-cli once: `cargo install sqlx-cli --no-default-features --features sqlite,native-tls`.
  2. Point `DATABASE_URL` at a local SQLite file (`.env.example` documents the
     default of `sqlite://./dev.db`; `.env` is gitignored).
  3. `cargo sqlx database create` (first time) and apply migrations.
  4. After editing any `query!` invocation, run
     `cargo sqlx prepare --workspace -- --tests` and commit the `.sqlx/`
     diff alongside the code change.
- **Async commits.** All database access is async. Synchronous database
  helpers are not an option; code that needs a quick query inside a sync
  context has to surface the asynchrony to its caller.
- **Migration model is fixed** to `sqlx::migrate!`: forward-only, embedded in
  the binary, run on startup. Picked here so F-188 inherits it rather than
  re-deciding.
- **Postgres remains a future option.** If `kino-vision.md` §8 ever resolves
  in favour of Postgres, the change is mostly schema and feature flags, not a
  rewrite of every call site.
- **No `unwrap`/`expect` carve-out.** sqlx errors flow through the per-crate
  `kino_db::Error` (with `#[from] sqlx::Error`), matching the workspace
  convention in CLAUDE.md.
