# ADR-0002 — Request UX semantics: visible-on-demand fulfillment

**Status:** Accepted
**Date:** 2026-05-09

## Context

A `Request` is the central object of Kino's fulfillment pipeline
(`kino-vision.md` §5). Its visibility shapes three things at once: the data
model in `kino-db`, the internal API in `kino-fulfillment`, and the
client-facing API that `kino-server` will expose in Phase 2. Once a
client API ships, its shape is hard to revisit without breaking native
clients on tvOS, iOS, and macOS.

The question — open in `kino-vision.md` §8 and tracked as Linear F-232 —
is how much of fulfillment a client sees:

- **A. Fully visible.** The client sees the chosen plan, the active
  provider, transient errors, and retry attempts. Maximum transparency,
  but the API surface fans out to mirror every internal state, and
  provider names become part of the public contract.
- **B. Black box.** The client sees `pending`, `satisfied`, or `failed`,
  and nothing else. Smallest API surface; a stuck request is opaque, and
  the operator (who in a self-host deployment is the same person as the
  user) has nowhere to look from the client.
- **C. Visible-on-demand.** The default projection is narrow and
  outcome-shaped; a separate "detail" projection exposes plan, provider
  attempts, and the event log for users who want to debug.

Two of Kino's vision principles (`kino-vision.md` §9) push the same
direction. *"Owned end-to-end beats integrated"* makes B unattractive:
the value Kino offers over a patchwork stack is exactly that the user
can see across the whole pipeline when something is wrong. *"Clear
separation of orchestration and acquisition"* makes A unattractive: the
default client contract should describe what Kino orchestrates (a
request reaching a state), not which acquisition provider was chosen for
this particular request — that detail is configurable, replaceable, and
should not become load-bearing public API.

The status-event audit trail required by F-220 is being stored
regardless of this decision. The remaining choice is which slice of it
clients see by default.

## Decision

Adopt option C. Define two projections of a `Request`:

**Default projection.** The shape every internal caller and (in Phase 2)
every client receives unless detail is explicitly requested:

- `id` (`kino_core::Id`)
- `state` — the documented state-machine value: `Pending`, `Resolved`,
  `Planning`, `Fulfilling`, `Ingesting`, `Satisfied`, `Failed`,
  `Cancelled`.
- `media_identity` — the canonical TMDB/TVDB resolution, once
  `state` has advanced past `Pending`. `None` until then.
- `created_at`, `updated_at` — `kino_core::Timestamp`.
- `failure_reason` — present iff `state = Failed`. A typed enum with a
  bounded, user-facing variant set (e.g. `NoProviderAccepted`,
  `AcquisitionFailed`, `IngestFailed`, `Cancelled`); not a free-form
  upstream error string.

The default projection deliberately does **not** name which provider was
selected, expose retry counts, or echo upstream errors.

**Detail projection.** Opt-in, surfaced behind an explicit endpoint or
flag (the precise transport is left to the API ADR; the contract is
fixed here):

- The default projection, plus
- The complete status-event log (the F-220 audit trail).
- The active fulfillment plan, including the provider currently
  attempting fulfillment and any prior providers that were tried.
- Per-provider-attempt outcome and last error, summarised — not raw
  upstream payloads.
- Internal correlation IDs (`Id`-typed) sufficient to cross-reference
  log lines.

**Never exposed to clients, in either projection:** provider credentials
or configuration values, raw upstream error payloads, panic messages or
stack traces, and any internal identifier that is not a `kino_core::Id`.
These remain visible only via `tracing` output to the operator's log
sink.

The internal `kino-fulfillment` API used inside the server process MAY
return both projections from the same call (e.g. a single struct with
the detail fields behind an `Option`); the projection split is a
property of the *external* contract that `kino-server` exposes.

## Consequences

- **Phase 1 (internal Request API, F-220 sibling) commits to this
  shape.** The internal API is allowed to be a single rich struct, but
  must keep the default-projection fields cleanly separable so that
  `kino-server` can serialise either without the type assembling fields
  it then has to filter out.
- **Phase 2 client API has two endpoints (or one endpoint with a flag).**
  The default endpoint is the stable contract that native clients build
  against; the detail endpoint is allowed to evolve as the fulfillment
  pipeline grows. Treat default-projection field changes as breaking;
  detail-projection field additions as non-breaking.
- **Provider names are not public API.** Adding, removing, or renaming a
  fulfillment provider does not require a client release. This protects
  the vision principle that providers are user-configured edges, not
  first-class concepts in the client UX.
- **`failure_reason` is a typed enum, owned by `kino-fulfillment`.** New
  failure variants are additive; mapping from internal errors to public
  variants happens at the crate boundary, in line with CLAUDE.md's "errors
  at crate boundaries are the boundary" rule. Adding a new
  internal-error path therefore forces a deliberate choice about whether
  it surfaces to clients and as which variant.
- **Operator visibility is preserved.** Anything the default projection
  hides remains accessible via the detail projection and via `tracing`
  logs at `debug` level. The black-box failure mode (B) is avoided.
- **Audit-trail storage is unchanged.** Status events are written and
  retained regardless of which projection a given caller reads. This
  decision affects the read-side surface, not the write-side schema.
- **Resolves `kino-vision.md` §8 "Request UX semantics".** That bullet
  is removed from the open-questions list; the resolution is recorded
  here and indexed in `docs/adrs/README.md`.
