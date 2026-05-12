CREATE TABLE ephemeral_transcodes (
    id              BLOB PRIMARY KEY,
    source_file_id  BLOB NOT NULL REFERENCES source_files(id) ON DELETE CASCADE,
    profile_hash    BLOB NOT NULL,
    profile_json    TEXT NOT NULL,
    directory_path  TEXT NOT NULL,
    size_bytes      INTEGER NOT NULL,
    created_at      INTEGER NOT NULL,
    last_access_at  INTEGER NOT NULL,
    UNIQUE(source_file_id, profile_hash)
);

CREATE INDEX ephemeral_transcodes_lru
    ON ephemeral_transcodes(last_access_at);
