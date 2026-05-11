# Media, Source, and Transcode Schemas

Linear issue: F-258

## Context

F-241 introduced an early `media_items` table so request fulfillment could
detect already-satisfied canonical identities. F-260 added the minimal
`source_files` table needed for scan reconciliation. This issue completes the
Phase 1 catalog schema without changing migration history.

## Scope

`kino-core` owns the shared catalog data model:

- `MediaItem` is the canonical user-facing catalog object.
- `SourceFile` is an original ingested file owned by a media item.
- `TranscodeOutput` is a derived stream output owned by a source file.

The database schema preserves existing rows by rebuilding the early catalog
tables in a new forward migration:

- `media_items` supports `movie`, `tv_episode`, and `personal` rows.
- Movies and TV episodes require a canonical identity.
- TV episodes are keyed by canonical series identity plus season and episode.
- Personal media has no canonical identity or episode locator.
- `source_files.media_item_id` references `media_items.id`.
- `transcode_outputs.source_file_id` references `source_files.id`.

## Notes

The old `tv_series` media-item value represented the provider identity rather
than a user-facing catalog object. The completed schema stores episode rows as
`tv_episode`; existing `tv_series` rows are migrated to `tv_episode` with
season and episode set to `1` because Phase 1 has no richer episode locator to
recover from the early table.

Transcode output rows are available in Phase 1 so downstream code can depend on
the schema, but population is still owned by the later transcoding phase.
