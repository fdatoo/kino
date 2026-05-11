# Phase 2 Roadmap — Playback Server

> Decomposition of the Phase 2 milestone into Linear epics and PR-sized sub-issues.
> Authored 2026-05-10 by Fynn Datoo + Claude (brainstorming session).
> Companion to `docs/agents/specs/2026-05-05-linear-roadmap-design.md`, which
> originally seeded Phase 2 as six flat stubs.

## 1. Goal

Translate Phase 2 of the Kino vision into a Linear plan that:

- Refines the six existing Phase 2 stub issues (F-262…F-267) into epic parents
  with concrete sub-issues.
- Adds the two epics the stubs miss: an OpenAPI contract surface (now load-bearing
  because Phase 2 introduces both a TypeScript admin UI and the eventual Phase 4
  Swift clients) and an image-subtitle OCR pipeline (now full-scope after the
  F-268 resolution).
- Resolves F-268 (subtitle policy) inline.
- Surfaces three new decision issues that must be settled inside Phase 2.
- Designs explicit Phase 2 → Phase 3 seams so the transcoding pipeline can land
  without reworking Phase 2 shapes.

The terminal state of executing this spec is a populated Linear workspace, not
code. Implementation work happens in subsequent sub-issues.

## 2. Inputs

- `docs/kino-vision.md` §5 (playback server) and §4 (architecture). Direct play
  is the steady state; transcode-on-the-fly is a Phase 3 backstop.
- `docs/agents/specs/2026-05-05-linear-roadmap-design.md` §5 — the original
  Phase 2 stubs.
- Phase 1 (`Phase 1 — Fulfillment & Library`, milestone
  `2b950a04-e227-41b4-a052-cbd10ea212e4`) shipped 2026-05-10. All 42 issues
  Done. Catalog, fulfillment, ingestion, and the transcode hand-off stub are in
  place.

## 3. Scope decisions

These were settled before decomposition. Each pin moved meaningfully more work
in or out of Phase 2.

| # | Question | Decision |
|---|---|---|
| 1 | Phase 2 / Phase 3 line on transcoding & variant selection | **Direct play only**, with a forward-looking variant contract Phase 3 plugs into. No on-the-fly transcoding in Phase 2. |
| 2 | User model given multi-user is Phase 5 | **User entity, single seeded user.** Schema + foreign keys exist from day one. Phase 5 adds creation/ACLs/sharing without schema breaks. |
| 3 | Admin UI stack | **TypeScript SPA bundled into the binary** (Vite + React recommended). Embedded via `include_dir!`, served by axum from `kino-admin`. |
| 4 | Client-facing API source of truth | **OpenAPI 3.1 generated from Rust handlers** via utoipa. TS client for the SPA; foundation for Phase 4 Swift clients. |
| 5 | F-268 subtitle policy | **Text + automatic OCR for all image subs at ingest.** Tesseract-style pipeline runs unconditionally for PGS/VOBSUB tracks; output stored as text sidecars and indexed through the existing `SubtitleService`. |
| 6 | Session management scope | **Observability only.** Sessions are recorded and queryable; no per-user or global concurrency limits. Limits land with multi-user in Phase 5. |

## 4. Phase 2 epics and sub-issues

Eight epics, ~46 sub-issues. Crate labels in `[brackets]`. Sub-issues marked
`DECISION` use the decision-issue body template and the `decision` label.

### Epic 1 — User & auth model

The substrate everything else attaches to. Lands early.

- Define `User` schema + single-seeded-user migration `[kino-core, kino-db]`
- Define `DeviceToken` schema (token hash, user FK, label, last_seen, revoked_at) `[kino-core, kino-db]`
- **DECISION:** Token format — opaque random vs. signed JWT `[decision, kino-server]`
- Implement token issuance API (mint, return plaintext once) `[kino-server]`
- Implement token revocation API `[kino-server]`
- Auth middleware (extract Bearer, resolve to user, attach to request extensions) `[kino-server]`
- Apply auth to existing catalog / request / admin endpoints `[kino-server]`

### Epic 2 — Client-facing catalog API

Promotes Phase 1's internal `/api/library/items` to a versioned external surface
and locks in the Phase 3 seams.

- Adopt `/api/v1` versioning policy and migrate the internal catalog routes `[kino-server]`
- Streamable-variant contract on item responses (Phase 2 ships one variant = source) `[kino-core, kino-server]`
- Full-text search across title + cast (SQLite FTS5) `[kino-db, kino-library]`
- Artwork local cache + serving (`/api/v1/library/items/{id}/images/{kind}`) `[kino-library, kino-server]`
- Filtering + pagination upgrades (sort orders, multi-field filters) `[kino-server]`
- Item detail endpoint with source-files + variant URLs `[kino-server]`
- Define `register_transcode_output(media_item_id, capabilities, file_path)` write API (no-op-acceptable stub; Phase 3 calls it) `[kino-library]`

### Epic 3 — OpenAPI contract & generated clients

New epic. Plumbing that pays back across the SPA (Phase 2) and the Swift
clients (Phase 4).

- Integrate utoipa + serve `/api/openapi.json` `[kino-server]`
- Annotate handlers; commit generated `openapi.json` to the repo `[kino-server]`
- CI: regenerate spec + fail on drift from committed copy `[ci]`
- TypeScript client generation build step in `kino-admin/web/` `[kino-admin]`

### Epic 4 — HLS streaming & direct play

- Range-request `GET /api/v1/stream/sourcefile/{id}` `[kino-server]`
- HLS master playlist (`/api/v1/stream/items/{id}/{variant_id}/master.m3u8`) `[kino-server]`
- HLS media playlist (byte-range segments off the source) `[kino-server]`
- WebVTT subtitle delivery (convert stored SRT/ASS/OCR → WebVTT on the fly) `[kino-library, kino-server]`
- Variant selection (`Match { variant_id }` | `NoSuitableVariant { reason }`; Phase 2 always matches source) `[kino-server]`
- Apply auth + open `PlaybackSession` on stream start `[kino-server]`

### Epic 5 — Playback state

- Define `PlaybackProgress` + `Watched` schemas (user_id, media_item_id, position_seconds, updated_at, source_device) `[kino-core, kino-db]`
- Heartbeat API: `POST /api/v1/playback/progress` `[kino-server]`
- Resume API: `GET /api/v1/playback/progress/{item_id}` `[kino-server]`
- Watched-flag transitions (auto at 90%; manual override API) `[kino-server]`
- **DECISION:** Cross-device sync conflict policy `[decision, kino-server]`

### Epic 6 — Session observability

- Define `PlaybackSession` schema (id, user_id, token_id, media_item_id, started_at, last_seen_at, status) `[kino-core, kino-db]`
- Session lifecycle: open on stream / heartbeat / idle close `[kino-server]`
- Background reaper (idle → ended transitions) `[kino-server]`
- Admin list-sessions API `[kino-server]`

### Epic 7 — Image subtitle OCR pipeline

New epic. F-268 resolved as automatic OCR; this is the work that resolution
implies.

- **DECISION:** OCR engine — Tesseract shell-out vs. Rust binding vs. cloud `[decision, kino-library]`
- Detect image-sub tracks during ingest probe (extends `ProbeSubtitleStream`) `[kino-fulfillment]`
- Extract image-sub tracks → image frames + timing `[kino-library]`
- Run OCR at ingest, persist as text sidecar `[kino-library]`
- Index OCR sidecars through existing `SubtitleService` `[kino-library]`
- Re-OCR as a deliberate, observable action (mirrors re-resolution) `[kino-library, kino-server]`

### Epic 8 — Admin web UI

- Pick + scaffold SPA framework (committed, not modeled as a decision; React recommended) `[kino-admin]`
- Vite build + `include_dir!`-baked asset embedding, served by axum `[kino-admin, kino-server]`
- Auth flow: token paste + per-device-token issuance screen `[kino-admin]`
- Library config screens (paths, providers shown read-only for Phase 2) `[kino-admin]`
- Manual import flow (consumes the existing F-251 endpoint) `[kino-admin]`
- Active session view (consumes Epic 6 API) `[kino-admin]`
- Re-OCR control surface (per-title trigger + failure surfacing) `[kino-admin]`

## 5. Phase 2 → Phase 3 hand-off

The four seams Phase 3 picks up without redesign:

1. **Variant contract.** Catalog item responses include a `variants: [{ variant_id, kind: "source" | "transcoded", capabilities: { codec, container, resolution, hdr }, stream_url }]` array. Phase 2 ships exactly one entry per item (`kind: "source"`). Phase 3 adds rows; no schema or response-shape change.
2. **Streaming endpoint signature.** `/api/v1/stream/items/{id}/{variant_id}/{master.m3u8 | segment/...}`. The variant id is an opaque identifier the catalog already returned in the item's `variants[]`. In Phase 2 every `variant_id` resolves to a `SourceFile`; in Phase 3 some resolve to `TranscodeOutput` rows instead. Same endpoint, same auth, same session lifecycle.
3. **Variant decision outcome.** The selector returns `Match { variant_id }` or `NoSuitableVariant { reason }`. Phase 2 always matches source. Phase 3 plugs an on-the-fly variant builder into the `NoSuitableVariant` branch — caller signature unchanged.
4. **Transcode-output registration.** A `register_transcode_output(media_item_id, capabilities, file_path)` write API in `kino-library`, specified in Phase 2 as a no-op-acceptable stub. Phase 3's pipeline calls it when a transcode job finishes. Avoids Phase 3 redesigning how outputs land in the catalog.

Phase 2 deliberately leaves the following for Phase 3: the transcode crate's
stub stays untouched, the `TranscodeOutput` table stays empty, on-the-fly
transcoding is not implemented, and codec/HDR-aware variant ranking is not yet
needed because there is only one variant.

## 6. Cross-cutting concerns Phase 2 introduces

- **Env vars** under the existing `KINO_` convention: `KINO_AUTH__TOKEN_*`, `KINO_STREAMING__*`, `KINO_OCR__ENGINE`, `KINO_SERVER__PUBLIC_BASE_URL` (needed for OpenAPI server entries). Documented in `kino.toml.example` alongside their defaults.
- **DB migrations** for `users`, `device_tokens`, `playback_progress`, `playback_sessions`, and either an `ocr_subtitle_sidecars` table or an extension of the existing subtitle index — implementer's call.
- **CI surface area** grows: pnpm install + Vite build + TypeScript typecheck for `kino-admin/web/`, plus an OpenAPI-drift check job. Cargo cache strategy already in place from Phase 0 covers the Rust side.
- **Tracing spans** wrap auth, streaming, OCR pipeline, and session lifecycle per the Phase 0 span conventions.
- **End-to-end test** mirroring Phase 1's `request_api_exercises_happy_path_end_to_end`: token issuance → list catalog → open stream → post progress heartbeat → list active sessions → mark watched. Lives under `crates/kino-server/tests/`.

## 7. Decisions surfaced

| # | Decision | Epic | Why it's a decision |
|---|---|---|---|
| 1 | Subtitle policy (F-268) | Epic 7 | Already an open issue; resolved by this spec to **text + automatic OCR**. F-268 is closed with the rationale rather than carried forward. |
| 2 | Token format — opaque random vs. signed JWT | Epic 1 | Affects revocation semantics and DB schema. |
| 3 | Cross-device sync conflict policy | Epic 5 | Last-writer-wins vs. max-position vs. device-aware. Affects API shape. |
| 4 | OCR engine | Epic 7 | Tesseract shell-out vs. Rust binding vs. cloud. Big dependency choice. |

Two committed-not-decision items follow the Phase 0 sqlx precedent (implementer
commits and moves on): `/api/v1` versioning policy, and SPA framework choice.

The three standing Phase 5 decisions (F-286 DB past v1, F-287 update strategy,
F-288 what "v1" means publicly) stay where they are.

## 8. Counts

- 8 epics — six are the existing F-262/263/264/265/266/267 stubs upgraded to
  epic parents; two are new (OpenAPI, OCR).
- 46 sub-issues — see §4.
- 3 new decision sub-issues inside Phase 2 epics (token format, sync policy,
  OCR engine); F-268 is resolved inline.

Roughly Phase 1's footprint (Phase 1 shipped 7 epics / 35 sub-issues; Phase 2
adds one extra epic and a similar sub-issue density).

## 9. Execution plan (when populating Linear)

Order matters because parent epics must exist before sub-issues, and the
existing stubs need to be reshaped rather than re-created.

1. Resolve F-268 inline — set status Done with the chosen option (text +
   automatic OCR) and the rationale paragraph from §3.
2. Update the existing six Phase 2 stubs (F-262, F-263, F-264, F-265, F-266,
   F-267) in place: replace the "to be detailed at the start of Phase 2"
   boilerplate with each epic's product capability and exit bar from §4.
3. Create the two new Phase 2 epic parents — **OpenAPI contract & generated
   clients** and **Image subtitle OCR pipeline** — milestoned to Phase 2 and
   labelled with their crate scopes.
4. Create the 46 sub-issues from §4 under their parent epics, with the crate
   labels applied. Three of these sub-issues use the decision-issue body
   template and the `decision` label.
5. Spot-check: every Phase 2 issue has the Phase 2 milestone, every sub-issue
   has a parent epic, every epic has at least one crate label, the three new
   decision issues are filterable by the `decision` label, and F-268 is closed
   with a resolution rather than left open.

## 10. Open questions deferred

None for this planning exercise. The six scope dials in §3 are settled. The
three decision sub-issues (token format, sync policy, OCR engine) are the first
work items to resolve once Phase 2 begins; they intentionally sit inside the
implementation phase rather than blocking it.
