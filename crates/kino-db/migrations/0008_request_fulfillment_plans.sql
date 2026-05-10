CREATE TABLE request_fulfillment_plans (
    id TEXT PRIMARY KEY NOT NULL,
    request_id TEXT NOT NULL REFERENCES requests(id) ON DELETE CASCADE,
    version INTEGER NOT NULL CHECK (version > 0),
    decision TEXT NOT NULL CHECK (
        decision IN (
            'already_satisfied',
            'needs_provider',
            'needs_user_input'
        )
    ),
    summary TEXT NOT NULL CHECK (summary != ''),
    status_event_id TEXT REFERENCES request_status_events(id) ON DELETE SET NULL,
    created_at TEXT NOT NULL,
    actor_kind TEXT CHECK (actor_kind IS NULL OR actor_kind IN ('system', 'user')),
    actor_id TEXT,
    UNIQUE (request_id, version),
    CHECK (
        (actor_kind IS NULL AND actor_id IS NULL)
        OR (actor_kind = 'system' AND actor_id IS NULL)
        OR (actor_kind = 'user' AND actor_id IS NOT NULL)
    )
);

CREATE INDEX request_fulfillment_plans_request_id_created_at_idx
ON request_fulfillment_plans (request_id, created_at, version);

CREATE TRIGGER request_fulfillment_plans_no_update
BEFORE UPDATE ON request_fulfillment_plans
BEGIN
    SELECT RAISE(ABORT, 'request fulfillment plans are append-only');
END;

CREATE TRIGGER request_fulfillment_plans_no_delete
BEFORE DELETE ON request_fulfillment_plans
BEGIN
    SELECT RAISE(ABORT, 'request fulfillment plans are append-only');
END;
