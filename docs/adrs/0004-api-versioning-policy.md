# ADR-0004 — API versioning policy

**Status:** Accepted
**Date:** 2026-05-11

## Context

Kino is moving from internal HTTP endpoints toward native clients that need a
durable contract. The existing request, catalog, and admin routes were mounted
directly under `/api`, which made it unclear whether a response shape was
temporary implementation detail or a stable client surface. Once tvOS, iOS, and
macOS clients ship, silently reshaping routes or payloads would force lockstep
client releases and make self-hosted upgrades risky.

## Decision

Use `/api/v1` as the stable client-facing API prefix. Requests, library catalog
reads, admin actions, streaming, playback, and future client routes live under
that prefix unless they are operational health or process-info endpoints.

Breaking API changes introduce a new major prefix, starting with `/api/v2`.
Breaking means removing or renaming a route, changing a field's type or meaning,
tightening accepted input, or reshaping a response in a way an existing client
cannot ignore. Kino never silently reshapes a versioned route in place.

Deprecations are surfaced in two places before removal: OpenAPI marks the route
or field with `deprecated: true`, and runtime responses include a `Deprecation`
HTTP header whose value cites the sunset version. The old version remains
available until that sunset version is released.

## Consequences

Clients can bind to `/api/v1` and treat additive fields and endpoints as normal
evolution. Server work that needs a breaking contract must either remain behind
a new route in the current version or open `/api/v2` deliberately. Maintaining
two major versions during a deprecation window adds server cost, but makes
client upgrades explicit and testable instead of accidental.
