CREATE TABLE pairings (
    id TEXT PRIMARY KEY NOT NULL,
    code TEXT NOT NULL UNIQUE,
    device_name TEXT NOT NULL,
    platform TEXT NOT NULL CHECK (platform IN ('ios', 'tvos', 'macos')),
    status TEXT NOT NULL CHECK (status IN ('pending', 'approved', 'expired', 'consumed')),
    token_id TEXT REFERENCES device_tokens(id),
    created_at TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    approved_at TEXT,
    CHECK (
        (
            status IN ('approved', 'consumed')
            AND token_id IS NOT NULL
            AND approved_at IS NOT NULL
        )
        OR (
            status NOT IN ('approved', 'consumed')
            AND token_id IS NULL
            AND approved_at IS NULL
        )
    )
);

CREATE INDEX pairings_status_expires
ON pairings(status, expires_at);
