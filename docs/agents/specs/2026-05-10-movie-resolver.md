# Movie Resolver

## Context

F-235 needs free-text movie requests to resolve to a ranked TMDB movie identity.
The codebase already has request-level match thresholds and a TV resolver that
accepts pre-fetched TMDB candidates. The TMDB HTTP client is still separate
work, so the movie resolver should not own network or credential handling.

TMDB's movie search endpoint is `GET /3/search/movie`; the useful resolver
fields are the movie `id`, display `title`, `release_date`, and `popularity`.

## Design

Add a `kino_fulfillment::movie` module with:

- typed TMDB movie ids
- a free-text parser for title plus optional trailing or parenthesized year
- a resolver that scores TMDB movie search results and returns either one
  resolved movie or an ambiguity/error

The scoring uses the same signals as request candidate scoring: title similarity,
year agreement, and popularity as a small tiebreaker. The resolver auto-selects
only when the top candidate clears the existing confidence threshold and margin.

## Rules

- Empty titles are rejected.
- A year is parsed only when it is trailing or parenthesized, so titles like
  `2001 A Space Odyssey` remain intact.
- `Inception 2010` resolves to TMDB id `27205` when that candidate is returned.
- `The Matrix` and `the matrix` resolve case-insensitively.
- Partial requests such as `Matrix` return ranked candidates instead of choosing
  silently when confidence is below the auto-resolve threshold.
