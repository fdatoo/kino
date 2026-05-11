# Subtitle Extraction Indexing

Linear issue: F-256

## Context

Phase 1 needs the library-side subtitle index that ingestion can call after a
file has been probed and placed. Full container probing and image subtitle OCR
policy are separate work. This slice stores extracted text subtitles as sidecar
files and records the languages available for each `MediaItem`.

## Scope

`kino-library` owns the subtitle extraction surface:

- `ProbedSubtitleTrack` represents a subtitle stream already discovered by the
  probe step, including language, stream index, detected format, and extracted
  text.
- `SubtitleService::extract_text_subtitles` writes SRT and ASS tracks to a
  caller-provided sidecar directory and persists one row per sidecar.
- PGS and VOBSUB tracks are treated as non-text formats and ignored until the
  Phase 2 subtitle-policy decision.
- `SubtitleService::subtitle_languages` returns distinct indexed languages for
  a media item.

The initial implementation writes text content it is handed. A future extractor
can replace that upstream text source with FFmpeg stream extraction without
changing the library-side index.

## Persistence

The database adds `subtitle_sidecars`, keyed to `media_items` with cascading
deletes. Each row stores the media item, normalized language, text subtitle
format, probed track index, sidecar path, and audit timestamps. A unique key on
`(media_item_id, language, format, track_index)` prevents duplicate index rows
for the same probed stream.
