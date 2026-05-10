CREATE TABLE media_items (
    id TEXT PRIMARY KEY NOT NULL,
    media_kind TEXT NOT NULL CHECK (media_kind IN ('movie', 'tv_series', 'personal')),
    canonical_identity_id TEXT UNIQUE REFERENCES canonical_identities(id) ON DELETE RESTRICT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    CHECK (
        (media_kind = 'personal' AND canonical_identity_id IS NULL)
        OR (media_kind != 'personal' AND canonical_identity_id IS NOT NULL)
    )
);

CREATE INDEX media_items_media_kind_created_at_idx
ON media_items (media_kind, created_at, id);
