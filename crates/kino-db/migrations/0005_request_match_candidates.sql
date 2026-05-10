PRAGMA defer_foreign_keys = ON;

ALTER TABLE request_status_events RENAME TO request_status_events_old;

ALTER TABLE requests RENAME TO requests_old;

CREATE TABLE requests (
    id TEXT PRIMARY KEY NOT NULL,
    state TEXT NOT NULL CHECK (
        state IN (
            'pending',
            'needs_disambiguation',
            'resolved',
            'planning',
            'fulfilling',
            'ingesting',
            'satisfied',
            'failed',
            'cancelled'
        )
    ),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    failure_reason TEXT CHECK (
        failure_reason IS NULL
        OR failure_reason IN (
            'no_provider_accepted',
            'acquisition_failed',
            'ingest_failed',
            'cancelled'
        )
    ),
    requester_kind TEXT NOT NULL DEFAULT 'anonymous'
    CHECK (requester_kind IN ('anonymous', 'system', 'user')),
    requester_id TEXT,
    target_raw_query TEXT NOT NULL DEFAULT '',
    canonical_identity_id TEXT,
    plan_id TEXT,
    CHECK (
        (state = 'failed' AND failure_reason IS NOT NULL)
        OR (state != 'failed' AND failure_reason IS NULL)
    )
);

INSERT INTO requests (
    id,
    state,
    created_at,
    updated_at,
    failure_reason,
    requester_kind,
    requester_id,
    target_raw_query,
    canonical_identity_id,
    plan_id
)
SELECT
    id,
    state,
    created_at,
    updated_at,
    failure_reason,
    requester_kind,
    requester_id,
    target_raw_query,
    canonical_identity_id,
    plan_id
FROM requests_old;

CREATE TABLE request_status_events (
    id TEXT PRIMARY KEY NOT NULL,
    request_id TEXT NOT NULL REFERENCES requests(id) ON DELETE CASCADE,
    from_state TEXT CHECK (
        from_state IS NULL
        OR from_state IN (
            'pending',
            'needs_disambiguation',
            'resolved',
            'planning',
            'fulfilling',
            'ingesting',
            'satisfied',
            'failed',
            'cancelled'
        )
    ),
    to_state TEXT NOT NULL CHECK (
        to_state IN (
            'pending',
            'needs_disambiguation',
            'resolved',
            'planning',
            'fulfilling',
            'ingesting',
            'satisfied',
            'failed',
            'cancelled'
        )
    ),
    occurred_at TEXT NOT NULL,
    message TEXT,
    actor_kind TEXT CHECK (actor_kind IS NULL OR actor_kind IN ('system', 'user')),
    actor_id TEXT,
    CHECK (from_state IS NULL OR from_state != to_state),
    CHECK (
        (actor_kind IS NULL AND actor_id IS NULL)
        OR (actor_kind = 'system' AND actor_id IS NULL)
        OR (actor_kind = 'user' AND actor_id IS NOT NULL)
    )
);

INSERT INTO request_status_events (
    id,
    request_id,
    from_state,
    to_state,
    occurred_at,
    message,
    actor_kind,
    actor_id
)
SELECT
    id,
    request_id,
    from_state,
    to_state,
    occurred_at,
    message,
    actor_kind,
    actor_id
FROM request_status_events_old;

DROP TABLE request_status_events_old;

DROP TABLE requests_old;

CREATE INDEX requests_created_at_id_idx
ON requests (created_at, id);

CREATE INDEX requests_state_created_at_id_idx
ON requests (state, created_at, id);

CREATE INDEX request_status_events_request_id_occurred_at_idx
ON request_status_events (request_id, occurred_at, id);

CREATE TRIGGER request_status_events_no_update
BEFORE UPDATE ON request_status_events
BEGIN
    SELECT RAISE(ABORT, 'request status events are append-only');
END;

CREATE TRIGGER request_status_events_no_delete
BEFORE DELETE ON request_status_events
BEGIN
    SELECT RAISE(ABORT, 'request status events are append-only');
END;

CREATE TABLE request_match_candidates (
    request_id TEXT NOT NULL REFERENCES requests(id) ON DELETE CASCADE,
    rank INTEGER NOT NULL CHECK (rank > 0),
    canonical_identity_id TEXT NOT NULL,
    title TEXT NOT NULL CHECK (title != ''),
    year INTEGER,
    popularity REAL NOT NULL CHECK (popularity >= 0),
    score REAL NOT NULL CHECK (score >= 0 AND score <= 1),
    created_at TEXT NOT NULL,
    PRIMARY KEY (request_id, rank),
    UNIQUE (request_id, canonical_identity_id)
);

CREATE INDEX request_match_candidates_request_id_score_idx
ON request_match_candidates (request_id, score DESC, rank);
