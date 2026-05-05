# Linear Roadmap — Kino Project Plan

> Translation of `docs/kino-vision.md` into a populated Linear workspace.
> Authored 2026-05-05 by Fynn Datoo + Claude (PM brainstorming session).

## 1. Goal

Translate the Kino vision document into a Linear plan that:

- Reflects the 5-phase roadmap from the vision, plus a Phase 0 for foundations.
- Captures Phase 0 and Phase 1 in PR-sized work; leaves Phases 2–5 as epic stubs to be detailed at the start of each phase.
- Surfaces the §8 open questions as forcing-function `decision` issues, milestoned to the phase where they must be resolved.
- Encodes agent-accountability scaffolding (CLAUDE.md, AGENTS.md, git hooks, GitHub Actions, semantic one-line commits) as concrete Phase 0 work.

The terminal state of *executing* this spec is a populated Linear workspace, not code.

## 2. Linear structure

### Project

A single Linear project, **Kino**, in the FynnLabs team. Already exists; gets a description update synthesizing the vision.

### Milestones (6)

| # | Milestone | Detail level today |
|---|-----------|--------------------|
| 0 | Phase 0 — Foundations | Fully detailed |
| 1 | Phase 1 — Fulfillment & Library | Fully detailed |
| 2 | Phase 2 — Playback Server | Epic stubs only |
| 3 | Phase 3 — Transcoding Pipeline | Epic stubs only |
| 4 | Phase 4 — Native Apple Clients | Epic stubs only |
| 5 | Phase 5 — Polish | Epic stubs only |

Each milestone gets a description with its **exit bar** (the "good enough to use daily for that slice" threshold).

### Labels (10)

**Crate / surface area (8):** `kino-core`, `kino-db`, `kino-fulfillment`, `kino-library`, `kino-transcode`, `kino-server`, `kino-admin`, `clients`

**Workflow modifiers (2):**
- `decision` — open question or committed decision needing a forcing-function issue.
- `early-client-spike` — Phase 4 work pulled forward (e.g. during Phase 2) to validate the API.

Deliberately omitted: `infra`, `research`, `bug`, per-platform client splits, type-of-work labels. Filtering by parent epic covers most queries.

### Hierarchy

Two levels: **Phase milestone → Epic (parent issue) → Sub-issue (concrete work)**.

- **Epic naming:** noun phrase scoped to a module (e.g. "Request tracking", "Provider interface").
- **Sub-issue naming:** verb-first, completable in 1–3 days (e.g. "Define Request schema", "Implement TMDB resolver").
- **Tests and docs are part of the work, not separate issues.** A sub-issue isn't done until tests pass and any decision-record updates are written.

### Process overhead — none

No priorities, estimates, or cycles. Phases provide ordering. Anything finer-grained is fiction at this stage.

### Decision issue body template

```
**Question:** <one sentence>

**Context:** <why this needs deciding now / what work depends on it>

**Options considered:**
- A. <option> — <tradeoffs>
- B. <option> — <tradeoffs>
- C. <option> — <tradeoffs>

**Status:** Open | Resolved YYYY-MM-DD

**Resolution:** <only when resolved — the chosen option + rationale>
```

### What's NOT in Linear

- The vision document (lives in `docs/kino-vision.md`).
- Architecture decision records — live in `docs/` next to the code, referenced from decision issues when resolved.
- Per-task TODO checklists — handled in PRs, not Linear.

## 3. Phase 0 — Foundations (detailed)

**Exit bar:** A fresh clone runs `just setup` (or equivalent) to install hooks, then `cargo build` + `cargo test` succeed. Pushing a PR with a clippy violation or unformatted code fails CI. Pre-commit blocks the same locally. CLAUDE.md and AGENTS.md describe the agent-accountability rules. The `kino` binary boots, loads config, initializes tracing, connects to a fresh SQLite database with migrations applied, logs "ready," and exits.

### Epic 1 — Workspace bootstrap
- Initialize Cargo workspace + shared dependency + workspace lint config
- Scaffold all crate skeletons (`kino-core`, `kino-db`, `kino-fulfillment`, `kino-library`, `kino-transcode`, `kino-server`, `kino-admin`, `kino-cli`)
- `kino` binary entry point — initializes config + logging, does nothing else
- License + README skeleton

### Epic 2 — `kino-core` scaffolding
- Error type strategy (`thiserror` for library errors at crate boundaries, no `anyhow` in library code)
- Config loader (file + env, `serde`-based, validated at startup, fail-fast on missing required fields)
- Shared time + id types (UUID v7 for ids, UTC `OffsetDateTime` for timestamps)

### Epic 3 — `kino-db` harness
- Pick DB driver + commit (recommendation: `sqlx` with offline mode for compile-time checked queries; not modeled as a `decision` issue — implementer commits and moves on)
- Connection pool setup with sane defaults for a single-binary deployment
- Migration runner — forward-only, embedded in binary, runs on startup
- Test fixture pattern (per-test in-memory DB; helper to apply migrations + seed)

### Epic 4 — Logging & tracing
- `tracing` subscriber: human-readable in dev, JSON in prod, env-controlled
- Span conventions (request-id propagation, instrumentation patterns)
- Log-level guidelines documented (what goes at info / debug / warn / error)

### Epic 5 — GitHub Actions
- Workflow: clippy (`--deny warnings`), rustfmt (`--check`), test (`cargo test --workspace`), build
- Cargo cache strategy (registry + target dirs, keyed on `Cargo.lock`)
- Branch protection on `main` — required checks must pass before merge
- Release build job (debug + release, Linux x86_64 primary target)

### Epic 6 — Git hooks (agent accountability)
- Pre-commit hook script — runs `cargo fmt --check` and `cargo clippy` on staged Rust files
- Hooks committed to `.githooks/` and activated via `git config core.hooksPath .githooks` in a setup task
- Setup task lives in a `Justfile` or `Makefile` so agents and humans run the same one
- Documented in CLAUDE.md and AGENTS.md as required first step

### Epic 7 — Agent guidance & repo hygiene
- `CLAUDE.md` at repo root — Claude Code–specific guidance, conventions, commands
- `AGENTS.md` at repo root — generic agent guidance (mirrors CLAUDE.md content where it overlaps)
- `docs/agents/` directory created with `specs/` and `plans/` subdirs and a README explaining the layout
- `docs/` reserved for product + architecture documentation; agent docs stay out
- Commit message convention documented: **semantic prefix, one line maximum** (e.g. `feat(fulfillment): add request state machine`) — no body, no Claude/Co-Authored-By trailers, ever
- `.editorconfig`, `.gitignore` (Rust + macOS + common IDEs)

## 4. Phase 1 — Fulfillment & Library (detailed)

**Exit bar:** Given a configured provider, a user request flows end-to-end: request → resolved canonical identity → plan → provider acquires file → ingestion places + enriches → catalog returns it on read. A stub transcode hand-off exists. No clients, no streaming yet.

### Epic 1 — Request tracking
*The data model + state machine for "I want this in my library."*
- Define Request data model (schema, fields, foreign keys) `[kino-core, kino-db]`
- Implement Request persistence + CRUD `[kino-db]`
- Implement Request state machine (pending → resolved → planning → fulfilling → ingesting → satisfied / failed / cancelled) `[kino-fulfillment]`
- Internal Request API (server-side; no client surface yet) `[kino-server, kino-fulfillment]`
- Request status events (durable audit trail of state changes) `[kino-fulfillment]`
- **DECISION:** Request UX semantics — black-box vs. visible fulfillment `[decision, kino-fulfillment]`

### Epic 2 — Resolution
*Map a request to a canonical TMDB/TVDB identity.*
- Define canonical media identity model (TMDB id as primary key, source provenance) `[kino-core, kino-db]`
- TMDB client (HTTP, auth, rate-limit handling, response caching) `[kino-fulfillment]`
- Movie resolver (title/year → TMDB movie) `[kino-fulfillment]`
- TV resolver (title/year → TMDB series + season/episode) `[kino-fulfillment]`
- Match scoring + ambiguity handling (low-confidence requires user disambiguation) `[kino-fulfillment]`
- Re-resolution as a deliberate, observable action (versioned, audited, never silent) `[kino-fulfillment]`
- **DECISION:** Metadata provider strategy beyond TMDB (anime, non-English, home video) `[decision, kino-library]`

### Epic 3 — Fulfillment planning
*"Given this request and the current library, what should we do?"*
- Compute fulfillment plan (returns: already-satisfied | needs-provider | needs-user-input) `[kino-fulfillment]`
- "Already satisfied" detection (request maps to an existing MediaItem) `[kino-fulfillment]`
- Provider selection logic (rank configured providers by capability + user preference) `[kino-fulfillment]`
- Plan persistence + observability (user can see Kino's decision, not just its outcome) `[kino-fulfillment]`

### Epic 4 — Provider interface
*The narrow, stable contract that lets users plug in their own acquisition tooling.*
- Define `FulfillmentProvider` trait `[kino-core, kino-fulfillment]`
- Provider configuration model (per-provider config in the main config file) `[kino-core]`
- Provider capability declaration (what request shapes it claims it can satisfy) `[kino-fulfillment]`
- Provider lifecycle (start fulfillment / status / cancel) `[kino-fulfillment]`
- Provider error model (transient vs. permanent, retry policy) `[kino-fulfillment]`

### Epic 5 — First-party providers
*The three Kino ships in the box.*
- Disc rip import provider (consume MakeMKV output directory) `[kino-fulfillment]`
- Watch folder provider (filesystem watcher + file-stability detection) `[kino-fulfillment]`
- Manual import provider (admin-triggered "ingest this path") `[kino-fulfillment, kino-admin]`

### Epic 6 — Ingestion pipeline
*Files-on-disk → catalog entries.*
- File probe + identification (container, codecs, duration, language tracks) `[kino-fulfillment]`
- Match probed file to its request (verify it satisfies what was asked) `[kino-fulfillment]`
- Metadata enrichment (TMDB images, descriptions, cast — write-through cached) `[kino-library]`
- Canonical layout writer (move/link file into `Library/Movies/Title (Year)/...`) `[kino-library]`
- Subtitle extraction + indexing (text subs only for v1; OCR is a Phase-2 decision) `[kino-library]`
- Hand-off interface to transcode (stub for Phase 1; real impl in Phase 3) `[kino-fulfillment, kino-transcode]`

### Epic 7 — Library catalog
*The canonical, queryable view of what exists.*
- Define `MediaItem`, `SourceFile`, `TranscodeOutput` schemas `[kino-core, kino-db]`
- Catalog read API (list, filter, get-by-id; internal for now) `[kino-server, kino-library]`
- Library scan + reconciliation (detect out-of-band filesystem changes) `[kino-library]`
- Storage layout policy (Kino owns the directory; import is one-way) `[kino-library]`

## 5. Phases 2–5 (epic stubs)

### Phase 2 — Playback Server
**Exit bar:** Kino can be pointed at a library and serve it to a generic HLS client.
- Catalog API (external, client-facing) — extends Phase 1's internal API
- HLS streaming + byte-range fallback for direct play
- Auth & per-device tokens
- Playback state (resume, watched flags; per-user; cross-device sync)
- Session management
- Admin web UI (minimal — config, ops, manual import)
- **DECISION:** Subtitle policy — OCR / forced-sub detection scope

### Phase 3 — Transcoding Pipeline
**Exit bar:** New files land transcoded; the output-set policy is realized end-to-end.
- Transcode job model & queue
- Probing & decision logic (per-title output set)
- FFmpeg orchestration
- Hardware acceleration detection & dispatch (VAAPI/NVENC/VideoToolbox/QSV) with software fallback
- Codec policy (AV1 → HEVC → H.264 fallback chain)
- HDR preservation + tone-mapping path
- Idempotency & resumability
- Integration with Phase 1's transcode hand-off stub

### Phase 4 — Native Apple Clients
**Exit bar:** Usable native clients on iOS, tvOS, macOS — sequenced iOS → tvOS → macOS.
- Shared Swift package (API bindings, models, playback coordination)
- iOS client
- tvOS client
- macOS client
- API validation spike (Phase 2-era; `early-client-spike` label)

### Phase 5 — Polish
**Exit bar:** "Works for the household," not just "works for me."
- Multi-user (accounts, per-user state, ACLs)
- Sharing
- Remote access hardening
- Observability (metrics, traces, structured logging conventions)
- **DECISION:** Database choice past v1 (Postgres?)
- **DECISION:** Update & migration strategy across breaking schema changes
- **DECISION:** What "v1" means publicly (release bar, docs, support, breaking-change policy)

## 6. §8 decision placement

| # | Question | Phase | Epic |
|---|---|---|---|
| 1 | DB past v1 | 5 | own decision issue |
| 2 | Subtitle policy | 2 | own decision issue |
| 3 | Metadata provider strategy | 1 | Resolution |
| 4 | Update/migration strategy | 5 | own decision issue |
| 5 | Request UX semantics | 1 | Request tracking |
| 6 | What "v1" means publicly | 5 | own decision issue |

## 7. Counts

- 1 project (description updated, not created)
- 6 milestones
- 37 epics today: 7 in Phase 0 + 7 in Phase 1 + 23 stubs across Phases 2–5 (Phase 2: 6, Phase 3: 8, Phase 4: 5, Phase 5: 4)
- ~60 sub-issues today (~25 in Phase 0, ~35 in Phase 1)
- 6 decision issues — 2 live as sub-issues inside Phase 1 epics (Request UX semantics under Request tracking, Metadata provider strategy under Resolution); 4 are standalone, milestoned issues with no epic parent (Phase 2: subtitle policy; Phase 5: DB past v1, migration strategy, "v1" meaning)

**Grand total: ~100 issues.**

## 8. Execution plan (when populating Linear)

Order matters because parent issues must exist before sub-issues, and milestones must exist before issues can be assigned to them.

1. Update Kino project description with vision synthesis.
2. Create the 10 labels.
3. Create the 6 milestones, each with its exit bar in the description.
4. Create epic parent issues for Phase 0 (7), assigned to the Phase 0 milestone.
5. Create sub-issues under each Phase 0 epic, with crate labels applied.
6. Create epic parent issues for Phase 1 (7), assigned to the Phase 1 milestone.
7. Create sub-issues under each Phase 1 epic, with crate labels applied. Two of these sub-issues use the decision-issue body template and the `decision` label (Request UX semantics, Metadata provider strategy).
8. Create epic stubs for Phases 2–5 (23 epics total, no sub-issues).
9. Create the 4 standalone decision issues with the body template and `decision` label, milestoned to Phase 2 (subtitle policy) and Phase 5 (DB past v1, migration strategy, "v1" meaning).
10. Spot-check: every issue has a milestone; every sub-issue has a parent; every epic has a crate label or labels; decision issues are filterable by the `decision` label.

## 9. Open questions deferred

None for this planning exercise. All structural decisions are locked.
