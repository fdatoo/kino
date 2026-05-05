# Working in Kino

This is the canonical agent-onboarding doc. `CLAUDE.md` is a symlink to it, so tools
that look for `AGENTS.md` (Codex, Copilot, Cursor, …) and tools that look for
`CLAUDE.md` (Claude Code) read the same content.

If you're a human, this doc still applies.

## Setup

A clean clone needs nothing beyond a working Rust toolchain. Standard commands:

| Action  | Command                                                |
|---------|--------------------------------------------------------|
| Build   | `cargo build --workspace`                              |
| Run     | `cargo run`                                            |
| Test    | `cargo test --workspace`                               |
| Format  | `cargo fmt --all`                                      |
| Lint    | `cargo clippy --workspace --all-targets -- -D warnings` |

Run all four (build, test, fmt-check, clippy) before claiming work is done — they
are what CI runs.

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

For non-trivial issues, write a design spec in `docs/agents/specs/` (named
`YYYY-MM-DD-short-slug.md`) before writing code. For multi-step execution, write a
plan in `docs/agents/plans/`.

## Code conventions

- **Rust edition 2024.** Workspace lints in `Cargo.toml` are authoritative
  (`unsafe_code = deny`, `clippy::all = warn`). Don't relax them locally.
- **Errors:** `thiserror` for library errors at crate boundaries. No `anyhow` in
  library code; `anyhow` is acceptable only in the top-level `kino` binary.
- **Logging:** `tracing` for everything. Human-readable in dev, JSON in prod,
  env-controlled. Don't `println!` in non-binary crates.
- **Time + ids:** UUID v7 for ids, UTC `OffsetDateTime` for timestamps. These types
  live in `kino-core` once added — depend on them, don't redefine them.
- **Tests:** colocated with the code (`#[cfg(test)] mod tests`) or under a crate's
  `tests/` directory for integration. A change isn't done until tests pass.

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
