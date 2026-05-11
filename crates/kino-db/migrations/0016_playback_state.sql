CREATE TABLE playback_progress (
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    media_item_id TEXT NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    position_seconds INTEGER NOT NULL CHECK (position_seconds >= 0),
    updated_at TEXT NOT NULL,
    source_device_token_id TEXT REFERENCES device_tokens(id) ON DELETE SET NULL,
    PRIMARY KEY (user_id, media_item_id)
);

CREATE INDEX playback_progress_user_id_idx
ON playback_progress (user_id);

CREATE TABLE watched (
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    media_item_id TEXT NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    watched_at TEXT NOT NULL,
    source TEXT NOT NULL CHECK (source IN ('auto', 'manual')),
    PRIMARY KEY (user_id, media_item_id)
);

CREATE INDEX watched_user_id_idx
ON watched (user_id);
