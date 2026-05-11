# Re-OCR Admin Action

## Goal

Re-running OCR for an existing image subtitle track is a deliberate admin action.
The action keeps the prior OCR sidecar queryable, marks it archived, and makes a
new JSON sidecar current for catalog responses.

## Design

`subtitle_sidecars.archived_at` records when a sidecar stopped being current.
Current rows are protected by a partial unique index on
`(media_item_id, language, format, track_index) WHERE archived_at IS NULL`, so
archived versions can retain the same logical track identity.

`SubtitleReocrService::reocr_track` looks up the current OCR sidecar for the
requested media item and track, uses the first source file attached to the media
item, runs image extraction and OCR through injectable traits, writes a
job-id-versioned JSON sidecar, archives the old row, and inserts the new current
row. The returned `job_id` is also the new sidecar id.

Catalog reads filter out archived sidecars. `SubtitleService::sidecars` still
returns archived rows so later diff or rollback workflows can inspect them.

## Limitation

Phase 2 runs `POST /api/v1/admin/items/{id}/subtitles/{track}/re-ocr`
synchronously inside the HTTP request. The response is still `202 Accepted` with
a tracking id so a later queue-backed implementation can keep the response shape.
