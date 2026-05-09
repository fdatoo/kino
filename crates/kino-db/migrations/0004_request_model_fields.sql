ALTER TABLE requests
ADD COLUMN requester_kind TEXT NOT NULL DEFAULT 'anonymous'
CHECK (requester_kind IN ('anonymous', 'system', 'user'));

ALTER TABLE requests
ADD COLUMN requester_id TEXT;

ALTER TABLE requests
ADD COLUMN target_raw_query TEXT NOT NULL DEFAULT '';

ALTER TABLE requests
ADD COLUMN canonical_identity_id TEXT;

ALTER TABLE requests
ADD COLUMN plan_id TEXT;
