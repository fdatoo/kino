CREATE TABLE request_identity_versions (
    request_id TEXT NOT NULL REFERENCES requests(id) ON DELETE CASCADE,
    version INTEGER NOT NULL CHECK (version > 0),
    canonical_identity_id TEXT NOT NULL,
    provenance TEXT NOT NULL CHECK (
        provenance IN ('match_scoring', 'manual')
    ),
    status_event_id TEXT,
    created_at TEXT NOT NULL,
    actor_kind TEXT CHECK (actor_kind IS NULL OR actor_kind IN ('system', 'user')),
    actor_id TEXT,
    PRIMARY KEY (request_id, version),
    CHECK (
        (actor_kind IS NULL AND actor_id IS NULL)
        OR (actor_kind = 'system' AND actor_id IS NULL)
        OR (actor_kind = 'user' AND actor_id IS NOT NULL)
    )
);

CREATE INDEX request_identity_versions_request_id_created_at_idx
ON request_identity_versions (request_id, created_at, version);

CREATE TRIGGER request_identity_versions_no_update
BEFORE UPDATE ON request_identity_versions
BEGIN
    SELECT RAISE(ABORT, 'request identity versions are append-only');
END;

CREATE TRIGGER request_identity_versions_no_delete
BEFORE DELETE ON request_identity_versions
BEGIN
    SELECT RAISE(ABORT, 'request identity versions are append-only');
END;
