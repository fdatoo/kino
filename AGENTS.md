# Working in Kino

This is the canonical agent-onboarding doc. `CLAUDE.md` is a symlink to it, so tools
that look for `AGENTS.md` (Codex, Copilot, Cursor, …) and tools that look for
`CLAUDE.md` (Claude Code) read the same content.

If you're a human, this doc still applies.

## Setup

First run `just setup`. Skipping this means your commits will likely be rejected.
This activates the mandatory Git hooks via `core.hooksPath`.

A clean clone also needs a working Rust toolchain and `just`. Standard commands:

| Action      | Command                                                |
|-------------|--------------------------------------------------------|
| Setup       | `just setup`                                           |
| Build       | `just build`                                           |
| Run         | `just run`                                             |
| Test        | `just test`                                            |
| Format      | `just fmt`                                             |
| Format check | `just fmt-check`                                      |
| Lint        | `just lint`                                            |

Run all four (`just build`, `just test`, `just fmt-check`, `just lint`) before
claiming work is done — they are what CI runs.

## Repo layout

```
Kino/
├── AGENTS.md              # this file (CLAUDE.md is a symlink to it)
├── Cargo.toml             # workspace root
├── crates/                # all Rust code (one crate per directory)
├── docs/                  # product, vision, architecture (long-lived)
└── docs/agents/           # agent-authored working docs
    ├── specs/             # design specs — what and why
    └── plans/             # implementation plans — how
```

Two rules:

- **Product/architecture docs go in `docs/`.** Agent-authored specs and plans
  go in `docs/agents/` so the product docs surface stays clean.
- **Crates own their own code.** No cross-crate `pub use` re-exports unless
  there's a real reason; a crate's surface is its `lib.rs`.

## Tracking

- **Linear** is the source of truth for what to build and in what order.
  - Project: **Kino** in **FynnLabs** team.
  - Issue identifiers look like `F-192`. Branch names follow Linear's
    `fdatoo/f-XXX-short-slug` format.
- **Vision:** `docs/kino-vision.md` — read this if anything below is unclear.
- **Roadmap context:** `docs/agents/specs/2026-05-05-linear-roadmap-design.md`.
- **ADRs:** `docs/adrs/` — cross-cutting architecture decisions (driver
  choices, wire formats, runtime model, etc.). See `docs/adrs/README.md`
  for when to write one.

For non-trivial issues, write a design spec in `docs/agents/specs/` (named
`YYYY-MM-DD-short-slug.md`) before writing code. For multi-step execution, write a
plan in `docs/agents/plans/`.

## Code conventions

- **Rust edition 2024.** Workspace lints in `Cargo.toml` are authoritative
  (`unsafe_code = deny`, `clippy::all = warn`). Don't relax them locally.
- **Errors:** Each library crate defines its own `Error` enum with `thiserror`.
  Variant `#[error("...")]` messages start lowercase, no trailing period. No
  `anyhow` in library code; it's allowed only in the top-level `kino` binary.
  No `unwrap`/`expect` outside tests — clippy's `unwrap_used` and `expect_used`
  workspace lints enforce this. There is no shared workspace-wide `Error` type;
  errors at crate boundaries are the boundary.
- **Logging:** `tracing` for everything. Human-readable in dev, JSON in prod,
  env-controlled. Don't `println!` in non-binary crates. Log at the lowest level
  that still reaches the right operator:
  - `info`: process and service lifecycle events, durable state transitions, and
    rare operational milestones that should be visible at the default log level
    (`server listening`, `database migrations complete`, `library scan started`).
    Don't use `info` for per-request chatter, tight loops, or expected branches.
  - `debug`: per-operation detail, branch decisions, retry attempts, counts, ids,
    and timings useful while diagnosing one request or job. Debug logs may be
    noisy when enabled, but they still need structured fields and bounded values.
  - `warn`: recoverable degradation where Kino continues but behavior changed or
    latency/correctness may be affected (`provider unavailable; using cache`,
    `scan skipped unreadable file`). Don't use `warn` for normal cache misses or
    other expected control flow.
  - `error`: bugs, invariant violations, data corruption, startup failures, and
    operation failures that the caller or supervisor must handle. Attach the
    error as a field (`error = %err` or `error = ?err`) and return/propagate it;
    logging is not a substitute for handling the error.
- **Time + ids:** Use `kino_core::Id` (UUID v7) and `kino_core::Timestamp` (UTC).
  Don't redefine them, don't construct ids with `Uuid::new_v4`, don't read clocks
  with `OffsetDateTime::now_utc()` directly — the newtypes own the invariants.
- **Docstrings & comments:** Every public item in a library crate gets a `///`
  doc comment — purpose and any non-obvious invariant ("always UTC", "always
  v7"), not the type signature. Every library crate gets a `//!` crate-level
  doc explaining its role. Inline `//` comments stay rare: the default is none,
  and one is added only when *why* is non-obvious (a constraint, a workaround,
  a surprising choice). Don't narrate *what* the code does. Tests and the
  top-level `kino` binary follow a lighter standard.
- **Configuration:** `kino_core::Config` is the single config type. The loader
  reads a TOML file (path: `KINO_CONFIG` env var, else `./kino.toml`) and layers
  `KINO_`-prefixed env vars on top. Nesting uses double underscores. Documented
  env vars: `KINO_CONFIG`, `KINO_DATABASE_PATH`, `KINO_LIBRARY_ROOT`,
  `KINO_LOG_LEVEL`, `KINO_LOG_FORMAT`, `KINO_LOG`, `RUST_LOG`,
  `KINO_SERVER__LISTEN`, `KINO_TMDB__API_KEY`,
  `KINO_TMDB__MAX_REQUESTS_PER_SECOND`,
  `KINO_PROVIDERS__WATCH_FOLDER__PATH`,
  `KINO_PROVIDERS__WATCH_FOLDER__PREFERENCE`. The reference TOML lives at
  `kino.toml.example` in the repo root.
- **Tests:** colocated with the code (`#[cfg(test)] mod tests`) or under a crate's
  `tests/` directory for integration. A change isn't done until tests pass.
- **DB tests:** DB-touching tests use `kino_db::test_db().await?`, which returns
  a fresh, fully migrated in-memory SQLite `Db`. Each call is isolated, so tests
  can run in parallel. Tests outside `kino-db` should enable the
  `kino-db/test-helpers` dev-dependency feature and use `write_pool()` for
  mutations plus `read_pool()` for read-path assertions.

## Commit messages

**Semantic prefix, one line maximum.** No body. No trailers. Ever.

```
feat(fulfillment): add request state machine
fix(db): handle migration table missing on first run
chore(ci): cache cargo target dir
docs(agents): document commit convention
```

- **Allowed prefixes:** `feat`, `fix`, `chore`, `refactor`, `test`, `docs`,
  `perf`, `build`.
- **Scope** is the crate name without the `kino-` prefix (e.g. `fulfillment`,
  `db`, `core`) or a top-level area (`ci`, `repo`, `agents`).
- **Subject** is imperative, lowercase, no trailing period.

**Never include:**

- Multi-line commit bodies. (Explanation belongs in the PR description or a spec.)
- `Co-Authored-By:` trailers.
- "🤖 Generated with Claude Code" footers, agent attribution, or tool watermarks
  of any kind.

If a change is too big for a one-line message, the change is too big — split it.

## Workflow expectations

1. **Read first:** the Linear issue, then any linked spec under
   `docs/agents/specs/`.
2. **Plan before coding** for non-trivial work — a short spec or plan under
   `docs/agents/`.
3. **Verify before claiming done:** run the four commands in [Setup](#setup)
   locally; treat their failure the same way CI will.
4. **One commit per logical change.** Don't bundle unrelated work into a single
   commit.
5. **Match the convention.** If you find yourself wanting to add a trailer or a
   commit body, re-read the [Commit messages](#commit-messages) section — it is
   not a default to override.
