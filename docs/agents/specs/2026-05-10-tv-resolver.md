# TV Resolver

## Context

F-236 needs free-text TV requests to resolve to a TMDB series identity, with
optional season and episode coordinates when the request includes them. The
current codebase has request tracking, match scoring, and identity versioning,
but does not yet have the TMDB HTTP client or canonical external-id persistence.

TMDB's current TV search endpoint is `GET /3/search/tv`; its useful resolver
fields are the series `id`, display `name`, `first_air_date`, and `popularity`.
Season and episode coordinates use TMDB's TV route shapes
`/3/tv/{series_id}/season/{season_number}` and
`/3/tv/{series_id}/season/{season_number}/episode/{episode_number}`.

## Design

Add a `kino_fulfillment::tv` module with:

- typed TMDB ids and TV episode locators
- a free-text parser for series requests and trailing `Sxx` or `SxxEyy`
  locators
- a resolver that scores TMDB TV search results and returns either one resolved
  series or an ambiguity/error

The resolver accepts already-fetched TMDB search candidates instead of performing
HTTP itself. This avoids inventing API-key/config wiring before the TMDB client
issue lands, while keeping the resolver contract shaped around the real TMDB
fields.

## Rules

- Empty titles are rejected.
- Season numbers may be zero so TMDB specials remain representable.
- Episode numbers must be positive.
- Season-only requests resolve to a series plus season locator; episode requests
  resolve to a series plus both season and episode locators.
- A top match resolves only when it clears the existing request match confidence
  threshold and margin.
- Ambiguous matches return ranked candidates instead of choosing silently.
