CREATE TABLE requests (
    id TEXT PRIMARY KEY NOT NULL,
    state TEXT NOT NULL CHECK (
        state IN (
            'pending',
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
    CHECK (
        (state = 'failed' AND failure_reason IS NOT NULL)
        OR (state != 'failed' AND failure_reason IS NULL)
    )
);

CREATE TABLE request_status_events (
    id TEXT PRIMARY KEY NOT NULL,
    request_id TEXT NOT NULL REFERENCES requests(id) ON DELETE CASCADE,
    from_state TEXT CHECK (
        from_state IS NULL
        OR from_state IN (
            'pending',
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
