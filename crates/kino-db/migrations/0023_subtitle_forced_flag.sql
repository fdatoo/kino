ALTER TABLE subtitle_sidecars
ADD COLUMN forced INTEGER NOT NULL DEFAULT 0
CHECK (forced IN (0, 1));
