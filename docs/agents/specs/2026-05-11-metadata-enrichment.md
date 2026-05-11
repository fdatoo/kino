# Metadata Enrichment

Linear issue: F-254

## Context

Ingestion needs a write-through metadata cache so catalog reads can serve rich
TMDB metadata from Kino's local database and filesystem. The fulfillment TMDB
client currently owns request resolution and an in-memory details cache; this
issue adds the library-side durable cache used after a `MediaItem` exists.

## Scope

`kino-library` owns metadata enrichment and readback:

- A metadata provider trait supplies TMDB metadata for a canonical identity.
- `MetadataService::enrich_tmdb_media_item` checks the durable cache first,
  fetches only on miss, writes image assets plus a metadata JSON sidecar to disk,
  and persists the cache rows.
- `MetadataService::cached_metadata` reads only from the local cache and does
  not need a TMDB provider.

The first durable cache stores poster, backdrop, optional logo, description,
release date, display title, and ordered cast. Asset files live under
`Metadata/tmdb/<media-kind>/<tmdb-id>/` below `library_root`.

## Persistence

Migration `0011_metadata_cache.sql` adds:

- `media_metadata_cache`, one row per `media_items.id`.
- `media_metadata_cast_members`, ordered cast rows keyed by media item.

The metadata row references both `media_items` and `canonical_identities`.
Deleting a media item cascades cache rows, while canonical identities remain
restricted like the rest of the request/library model.
