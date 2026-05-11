CREATE TABLE media_metadata_cache (
    media_item_id TEXT PRIMARY KEY NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    canonical_identity_id TEXT NOT NULL REFERENCES canonical_identities(id) ON DELETE RESTRICT,
    provider TEXT NOT NULL CHECK (provider IN ('tmdb')),
    title TEXT NOT NULL CHECK (title != ''),
    description TEXT NOT NULL CHECK (description != ''),
    release_date TEXT,
    poster_path TEXT NOT NULL,
    backdrop_path TEXT NOT NULL,
    logo_path TEXT,
    metadata_path TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX media_metadata_cache_identity_idx
ON media_metadata_cache (canonical_identity_id);

CREATE TABLE media_metadata_cast_members (
    media_item_id TEXT NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    position INTEGER NOT NULL CHECK (position >= 0),
    name TEXT NOT NULL CHECK (name != ''),
    character TEXT NOT NULL,
    profile_path TEXT,
    PRIMARY KEY (media_item_id, position)
);
