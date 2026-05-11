ALTER TABLE source_files
ADD COLUMN probe_duration_seconds INTEGER
CHECK (probe_duration_seconds IS NULL OR probe_duration_seconds > 0);

ALTER TABLE watched
ADD COLUMN unmarked INTEGER NOT NULL DEFAULT 0
CHECK (unmarked IN (0, 1));
