CREATE TABLE transcode_jobs (
    id              BLOB PRIMARY KEY,
    source_file_id  BLOB NOT NULL REFERENCES source_files(id) ON DELETE CASCADE,
    profile_json    TEXT NOT NULL,
    profile_hash    BLOB NOT NULL,
    state           TEXT NOT NULL,
    lane            TEXT NOT NULL,
    attempt         INTEGER NOT NULL DEFAULT 0,
    progress_pct    INTEGER,
    last_error      TEXT,
    next_attempt_at INTEGER,
    created_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL,
    started_at      INTEGER,
    completed_at    INTEGER,
    UNIQUE(source_file_id, profile_hash)
);

CREATE INDEX transcode_jobs_state_lane
    ON transcode_jobs(state, lane);

CREATE INDEX transcode_jobs_dispatch
    ON transcode_jobs(state, lane, next_attempt_at, created_at);
