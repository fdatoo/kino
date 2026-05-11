CREATE TABLE subtitle_sidecars (
    id TEXT PRIMARY KEY NOT NULL,
    media_item_id TEXT NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    language TEXT NOT NULL,
    format TEXT NOT NULL CHECK (format IN ('srt', 'ass')),
    track_index INTEGER NOT NULL CHECK (track_index >= 0),
    path TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE (media_item_id, language, format, track_index)
);

CREATE INDEX subtitle_sidecars_media_language_idx
ON subtitle_sidecars (media_item_id, language);
