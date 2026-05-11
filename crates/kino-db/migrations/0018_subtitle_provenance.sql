ALTER TABLE subtitle_sidecars RENAME TO subtitle_sidecars_old_0018;

DROP INDEX subtitle_sidecars_media_language_idx;

CREATE TABLE subtitle_sidecars (
    id TEXT PRIMARY KEY NOT NULL,
    media_item_id TEXT NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    language TEXT NOT NULL,
    format TEXT NOT NULL CHECK (format IN ('srt', 'ass', 'json')),
    provenance TEXT NOT NULL DEFAULT 'text' CHECK (provenance IN ('text', 'ocr')),
    track_index INTEGER NOT NULL CHECK (track_index >= 0),
    path TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE (media_item_id, language, format, track_index)
);

INSERT INTO subtitle_sidecars (
    id,
    media_item_id,
    language,
    format,
    provenance,
    track_index,
    path,
    created_at,
    updated_at
)
SELECT
    id,
    media_item_id,
    language,
    format,
    'text',
    track_index,
    path,
    created_at,
    updated_at
FROM subtitle_sidecars_old_0018;

CREATE INDEX subtitle_sidecars_media_language_idx
ON subtitle_sidecars (media_item_id, language);

DROP TABLE subtitle_sidecars_old_0018;
