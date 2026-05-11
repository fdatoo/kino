# Catalog Read API

Linear issue: F-259

## Context

Phase 1 needs an internal catalog read surface over `media_items` so admin and
orchestration code can inspect what Kino believes is present. The API is not a
client-facing library browser yet; Phase 2 can expand fields and presentation.

## Scope

`kino-library` owns catalog read queries:

- List media items with offset pagination.
- Get one media item by id.
- Include cached metadata title when enrichment has populated
  `media_metadata_cache`.
- Include source-file summaries from `source_files`.
- Filter by media kind, cached title substring, and source-file presence.

`kino-server` exposes the internal HTTP surface:

```text
GET /api/library/items
GET /api/library/items/{id}
```

Supported query parameters:

- `type=movie|tv|tv_episode|tv_series|personal`
- `title_contains=<text>`
- `has_source_file=true|false`
- `limit=<1..200>`
- `offset=<u64 within sqlite range>`

## Notes

Title filtering intentionally targets cached metadata titles. A media item
without enriched metadata has `title: null` and does not match
`title_contains`.

The API returns stable ids, media kind, canonical identity, optional title,
source-file rows, and timestamps. It does not expose client presentation fields
or playback state.

`tv_series` remains accepted as a legacy filter alias, but persisted media-item
rows use `tv_episode`.
