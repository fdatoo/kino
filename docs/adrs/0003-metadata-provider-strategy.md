# ADR-0003 — Metadata provider strategy: TMDB-only with overrides

**Status:** Accepted
**Date:** 2026-05-09

## Context

`kino-vision.md` §8 lists "Metadata provider strategy" as an open question:
TMDB is the obvious primary provider, but it has known gaps — anime
(episode numbering, Japanese metadata), non-English content (smaller
foreign films), and home video (which no public provider covers at all).
The vision explicitly calls out *"becoming a metadata provider
aggregator like Jellyfin did"* as the failure mode to avoid.

The decision — tracked as Linear F-239 — has to be made before the
canonical identity model in `kino-library` and the metadata-enrichment
sub-issue under the ingestion pipeline are locked in. Both depend on how
many providers a `MediaItem` can be associated with and what shape that
association takes.

The four options considered:

- **A. TMDB-only, accept the gaps.** Simplest, most opinionated. Anime
  and foreign content are best-effort. Home video has no answer.
- **B. TMDB + AniDB (hand-picked).** Two hardcoded providers covering
  the largest gap. Requires merge/precedence logic for two sources, two
  failure modes, two rate-limit budgets. Doesn't address foreign content
  or home video. Pays most of the abstraction cost without admitting it.
- **C. Pluggable metadata provider interface.** Mirrors the fulfillment
  provider pattern. Most flexible. With one real consumer (TMDB) any
  interface designed now encodes TMDB's shape; a real second provider
  would force it to be redone. The plugin-marketplace pathology that
  the vision warns about is a downstream consequence of this path.
- **D. TMDB + manual override.** Pragmatic. Per-item user corrections
  to TMDB output. Treats home video as a TMDB-without-a-match case,
  which is the wrong mental model: home video is not a TMDB *gap*, it
  is content TMDB has no business covering.

The framing reveals that the original four options conflate two
separate problems: (1) content TMDB covers poorly, where the question
is "should we call additional providers?", and (2) content for which no
public provider exists, where the question is "what does the catalog
look like when there is no provider call at all?". Treating these as
the same problem is what creates pressure to build a provider
interface.

A second axis the options also conflate is *runtime behaviour* (how
many providers Kino actually calls) versus *schema commitment* (whether
the canonical identity model names a specific provider). These are
separable, and the resolution treats them differently.

## Decision

Adopt a hybrid of A and D, with home video as a first-class media
category:

**Runtime — TMDB is the only metadata provider Kino calls.** No AniDB,
no TVDB, no IMDb, no Trakt, in v1 or in any subsequent release that
this ADR governs. There is no `MetadataProvider` trait in
`kino-library`; TMDB enrichment is a direct module, not an adapter.
There is no merge/precedence logic, no plugin surface, and no built-in
mechanism for users to register their own providers. Anime and
foreign-language libraries are explicitly not a v1 audience for the
parts of their experience that depend on data TMDB does not cover well.

**Per-item manual override is the escape hatch.** A `MediaItem` carries
an `overrides` map of field-level corrections that take precedence over
the TMDB-provided values. Overrides cover the cases where TMDB has the
item but is wrong about a specific field (episode title, episode
numbering, artwork, year). Overrides are user-supplied, persisted, and
preserved across re-resolution.

**Personal media is a distinct `MediaItem` kind.** A `MediaItem`
identifies as `Movie`, `Episode`, or `Personal`. `Personal` items have
no provider call, no canonical id, and rely entirely on user-supplied
metadata. They are not modelled as "a movie without a TMDB match"; the
catalog distinguishes the kinds at the type level so code paths that
assume a canonical id (re-resolution, artwork fetch, refresh) simply
do not apply to them.

**Schema — the canonical identity is provider-tagged but
single-valued.**

```text
MediaItem {
    kind: Movie | Episode | Personal,
    canonical_id: Option<ExternalId>,   // None when kind = Personal
    overrides: Map<Field, Value>,
    ...
}

ExternalId {
    provider: Provider,                 // enum, single variant: Tmdb
    id: String,
}
```

The `Provider` enum has one variant today (`Tmdb`). The schema does
not name TMDB at the column level (no `tmdb_id` field). This is a
deliberate concession to avoid baking a single provider's name into
the data model when the cost of doing so is trivial. It is **not** a
multi-provider abstraction: the catalog stores at most one external id
per item, no merge logic exists, and adding a second variant to the
enum requires a new ADR superseding this one.

## Consequences

- **`kino-library` ships with one enrichment path.** Calling TMDB,
  mapping its response into the catalog model, and applying overrides
  on read. There is no provider trait, no registry, no dispatch.
- **The canonical identity model is `Option<ExternalId>`, not
  `Option<TmdbId>`.** The persisted shape is `(provider, id)` with the
  provider stored as a stable string and read back into the
  single-variant enum. Adding a future provider is an enum addition and
  new code paths, not a column rename.
- **`Personal` is a first-class kind, not a fallback.** Code that
  consumes `MediaItem` discriminates on `kind` and does not assume a
  `canonical_id` is present except when kind is `Movie` or `Episode`.
  Operations that depend on a canonical id (re-resolve, refresh
  artwork, refresh metadata) are defined only for the provider-backed
  kinds.
- **Anime and foreign-content gaps are accepted.** The bar to add a
  second provider is a new ADR, not a code change. This commits the
  project to telling power users — including the project owner — that
  manual override is the only knob, and that re-opening the question
  is a deliberate decision, not drift.
- **The "aggregator" failure mode is foreclosed by absence of
  surface.** No `MetadataProvider` trait means no third-party providers
  to plug in, regardless of policy. The plugin-ecosystem non-goal in
  vision §3 is enforced by the type system rather than by convention.
- **Re-resolution remains a single-target operation.** When a TMDB
  match needs to change, the new identity replaces the old; there is no
  multi-source merge to reconcile. Overrides are preserved by field
  name across re-resolution, with conflicts (an override for a field
  the new TMDB record no longer exposes) handled at the
  re-resolution-pipeline level (out of scope for this ADR).
- **Home video and personal recordings are usable without external
  network calls.** Ingesting a `Personal` item is a fully local
  operation, unaffected by TMDB outages, rate limits, or API key
  configuration.
- **Resolves `kino-vision.md` §8 "Metadata provider strategy".** That
  bullet is removed from the open-questions list; the resolution is
  recorded here and indexed in `docs/adrs/README.md`.
