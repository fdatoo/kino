ALTER TABLE subtitle_sidecars RENAME TO subtitle_sidecars_old_0021;

DROP INDEX subtitle_sidecars_media_language_idx;

CREATE TABLE subtitle_sidecars (
    id TEXT PRIMARY KEY NOT NULL,
    media_item_id TEXT NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    language TEXT NOT NULL,
    format TEXT NOT NULL CHECK (format IN ('srt', 'ass', 'json')),
    provenance TEXT NOT NULL DEFAULT 'text' CHECK (provenance IN ('text', 'ocr')),
    track_index INTEGER NOT NULL CHECK (track_index >= 0),
    path TEXT NOT NULL,
    archived_at TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

INSERT INTO subtitle_sidecars (
    id,
    media_item_id,
    language,
    format,
    provenance,
    track_index,
    path,
    archived_at,
    created_at,
    updated_at
)
SELECT
    id,
    media_item_id,
    language,
    format,
    provenance,
    track_index,
    path,
    NULL,
    created_at,
    updated_at
FROM subtitle_sidecars_old_0021;

CREATE UNIQUE INDEX subtitle_sidecars_current_track_idx
ON subtitle_sidecars (media_item_id, language, format, track_index)
WHERE archived_at IS NULL;

CREATE INDEX subtitle_sidecars_media_language_idx
ON subtitle_sidecars (media_item_id, language);

DROP TABLE subtitle_sidecars_old_0021;
