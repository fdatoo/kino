# Canonical Identity Model

Linear: F-233

## Goal

Represent the canonical media target selected during resolution as a first-class
core type and database row. This identity is the stable answer to "which media
does the user mean?" before fulfillment and library catalog rows exist.

## Shape

`kino-core` owns the canonical identity value types:

- `CanonicalIdentityId` is the persisted primary key. Its string form is
  `tmdb:<kind>:<id>`, for example `tmdb:movie:550`.
- `CanonicalIdentityProvider` is deliberately single-variant today: `tmdb`.
- `CanonicalIdentityKind` separates TMDB movie and TV-series namespaces.
- `CanonicalIdentitySource` records what first introduced the identity:
  match scoring or a manual selection.
- `CanonicalIdentity` is the database projection for the identity row.

The key is deterministic instead of a generated UUID so the TMDB identity is the
canonical primary key. It is still provider- and kind-tagged because TMDB movie
and TV ids are not one shared namespace.

## Persistence

Migration `0007_canonical_identities.sql` creates `canonical_identities` and
rebuilds request-side tables so `requests.canonical_identity_id`,
`request_match_candidates.canonical_identity_id`, and
`request_identity_versions.canonical_identity_id` reference it.

Re-resolution versioning remains per request in `request_identity_versions`.
Changing request A from one canonical identity to another is an event in request
history, not a mutation of either canonical identity row.

`MediaItem` is not implemented yet. The migration sketches the intended catalog
relationship: provider-backed movie and episode rows will carry nullable
`canonical_identity_id REFERENCES canonical_identities(id)`, while personal media
will leave it absent.
