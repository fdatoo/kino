CREATE TABLE source_files (
    id TEXT PRIMARY KEY NOT NULL,
    media_item_id TEXT NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    path TEXT NOT NULL UNIQUE CHECK (path != ''),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX source_files_media_item_id_idx
ON source_files (media_item_id, id);
