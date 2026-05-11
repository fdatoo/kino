PRAGMA defer_foreign_keys = ON;

ALTER TABLE media_metadata_cast_members RENAME TO media_metadata_cast_members_old;
ALTER TABLE media_metadata_cache RENAME TO media_metadata_cache_old;
ALTER TABLE subtitle_sidecars RENAME TO subtitle_sidecars_old;
ALTER TABLE source_files RENAME TO source_files_old;
ALTER TABLE media_items RENAME TO media_items_old;

DROP INDEX media_metadata_cache_identity_idx;
DROP INDEX subtitle_sidecars_media_language_idx;
DROP INDEX source_files_media_item_id_idx;
DROP INDEX media_items_media_kind_created_at_idx;

CREATE TABLE media_items (
    id TEXT PRIMARY KEY NOT NULL,
    media_kind TEXT NOT NULL CHECK (media_kind IN ('movie', 'tv_episode', 'personal')),
    canonical_identity_id TEXT REFERENCES canonical_identities(id) ON DELETE RESTRICT,
    season_number INTEGER CHECK (season_number IS NULL OR season_number > 0),
    episode_number INTEGER CHECK (episode_number IS NULL OR episode_number > 0),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    CHECK (
        (
            media_kind = 'movie'
            AND canonical_identity_id IS NOT NULL
            AND season_number IS NULL
            AND episode_number IS NULL
        )
        OR (
            media_kind = 'tv_episode'
            AND canonical_identity_id IS NOT NULL
            AND season_number IS NOT NULL
            AND episode_number IS NOT NULL
        )
        OR (
            media_kind = 'personal'
            AND canonical_identity_id IS NULL
            AND season_number IS NULL
            AND episode_number IS NULL
        )
    )
);

INSERT INTO media_items (
    id,
    media_kind,
    canonical_identity_id,
    season_number,
    episode_number,
    created_at,
    updated_at
)
SELECT
    id,
    CASE media_kind
        WHEN 'tv_series' THEN 'tv_episode'
        ELSE media_kind
    END,
    canonical_identity_id,
    CASE media_kind
        WHEN 'tv_series' THEN 1
        ELSE NULL
    END,
    CASE media_kind
        WHEN 'tv_series' THEN 1
        ELSE NULL
    END,
    created_at,
    updated_at
FROM media_items_old;

CREATE UNIQUE INDEX media_items_movie_identity_idx
ON media_items (canonical_identity_id)
WHERE media_kind = 'movie';

CREATE UNIQUE INDEX media_items_tv_episode_identity_idx
ON media_items (canonical_identity_id, season_number, episode_number)
WHERE media_kind = 'tv_episode';

CREATE INDEX media_items_media_kind_created_at_idx
ON media_items (media_kind, created_at, id);

CREATE TABLE source_files (
    id TEXT PRIMARY KEY NOT NULL,
    media_item_id TEXT NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    path TEXT NOT NULL UNIQUE CHECK (path != ''),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

INSERT INTO source_files (
    id,
    media_item_id,
    path,
    created_at,
    updated_at
)
SELECT
    id,
    media_item_id,
    path,
    created_at,
    updated_at
FROM source_files_old;

CREATE INDEX source_files_media_item_id_idx
ON source_files (media_item_id, id);

CREATE TABLE transcode_outputs (
    id TEXT PRIMARY KEY NOT NULL,
    source_file_id TEXT NOT NULL REFERENCES source_files(id) ON DELETE CASCADE,
    path TEXT NOT NULL UNIQUE CHECK (path != ''),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX transcode_outputs_source_file_id_idx
ON transcode_outputs (source_file_id, id);

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

INSERT INTO subtitle_sidecars (
    id,
    media_item_id,
    language,
    format,
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
    track_index,
    path,
    created_at,
    updated_at
FROM subtitle_sidecars_old;

CREATE INDEX subtitle_sidecars_media_language_idx
ON subtitle_sidecars (media_item_id, language);

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

INSERT INTO media_metadata_cache (
    media_item_id,
    canonical_identity_id,
    provider,
    title,
    description,
    release_date,
    poster_path,
    backdrop_path,
    logo_path,
    metadata_path,
    created_at,
    updated_at
)
SELECT
    media_item_id,
    canonical_identity_id,
    provider,
    title,
    description,
    release_date,
    poster_path,
    backdrop_path,
    logo_path,
    metadata_path,
    created_at,
    updated_at
FROM media_metadata_cache_old;

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

INSERT INTO media_metadata_cast_members (
    media_item_id,
    position,
    name,
    character,
    profile_path
)
SELECT
    media_item_id,
    position,
    name,
    character,
    profile_path
FROM media_metadata_cast_members_old;

DROP TABLE media_metadata_cast_members_old;
DROP TABLE media_metadata_cache_old;
DROP TABLE subtitle_sidecars_old;
DROP TABLE source_files_old;
DROP TABLE media_items_old;
