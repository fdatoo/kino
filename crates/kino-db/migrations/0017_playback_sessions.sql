CREATE TABLE playback_sessions (
    id TEXT PRIMARY KEY NOT NULL,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    token_id TEXT NOT NULL REFERENCES device_tokens(id) ON DELETE CASCADE,
    media_item_id TEXT NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    variant_id TEXT NOT NULL CHECK (variant_id != ''),
    started_at TEXT NOT NULL,
    last_seen_at TEXT NOT NULL,
    ended_at TEXT,
    status TEXT NOT NULL CHECK (status IN ('active', 'idle', 'ended')),
    CHECK (
        (status = 'ended' AND ended_at IS NOT NULL)
        OR (status != 'ended' AND ended_at IS NULL)
    )
);

CREATE INDEX playback_sessions_status_last_seen_at_idx
ON playback_sessions (status, last_seen_at);
