CREATE TABLE device_tokens (
    id TEXT PRIMARY KEY NOT NULL,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    label TEXT NOT NULL CHECK (label != ''),
    hash TEXT NOT NULL UNIQUE,
    last_seen_at TEXT,
    revoked_at TEXT,
    created_at TEXT NOT NULL
);

CREATE INDEX device_tokens_user_id_idx
ON device_tokens (user_id, id);
