CREATE TABLE users (
    id TEXT PRIMARY KEY NOT NULL,
    display_name TEXT NOT NULL CHECK (display_name != ''),
    created_at TEXT NOT NULL
);

INSERT OR IGNORE INTO users (
    id,
    display_name,
    created_at
)
VALUES (
    '019e1455-6000-7000-8000-000000000001',
    'Owner',
    '2026-05-11T00:00:00Z'
);
