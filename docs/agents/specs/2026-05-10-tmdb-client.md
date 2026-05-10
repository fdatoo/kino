# TMDB Client

Linear: F-234

## Context

Movie and TV resolvers already accept TMDB-shaped search candidates. The missing
piece is the network client that performs authenticated TMDB calls, remains
polite under TMDB's changing rate limits, and avoids duplicate detail lookups
within one process session.

TMDB v3 supports application authentication with an `api_key` query parameter.
The current endpoints needed by resolution are:

- `GET /3/search/movie`
- `GET /3/search/tv`
- `GET /3/movie/{movie_id}`
- `GET /3/tv/{series_id}`

## Design

Add `kino_fulfillment::tmdb` as a direct TMDB client, not a provider trait. It
uses `reqwest`, reads API key and request-rate settings from `kino_core::Config`,
and returns the existing resolver search result types for search calls.

The client owns:

- a configured base URL
- a required API key
- a client-side rate gate, defaulting to 20 requests per second
- in-memory movie and TV detail caches keyed by TMDB id

## Rules

- Missing API keys are explicit client construction errors.
- Empty search queries are rejected before sending HTTP.
- Search calls authenticate and map TMDB responses into resolver candidates.
- Detail calls authenticate and cache successful responses for the lifetime of
  the client instance.
- `429 Too Many Requests` responses are retried with `Retry-After` seconds when
  present, otherwise exponential fallback backoff.
- Non-success HTTP responses return status plus response body instead of being
  silently discarded.
