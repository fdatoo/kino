CREATE TABLE media_items_fts_source (
    rowid INTEGER PRIMARY KEY,
    media_item_id TEXT NOT NULL UNIQUE REFERENCES media_items(id) ON DELETE CASCADE,
    title TEXT NOT NULL,
    cast_names TEXT NOT NULL
);

CREATE VIRTUAL TABLE media_items_fts USING fts5(
    title,
    cast_names,
    content='',
    tokenize='unicode61 remove_diacritics 2'
);

CREATE TRIGGER media_items_fts_source_ai
AFTER INSERT ON media_items_fts_source
BEGIN
    INSERT INTO media_items_fts (rowid, title, cast_names)
    VALUES (new.rowid, new.title, new.cast_names);
END;

CREATE TRIGGER media_items_fts_source_au
AFTER UPDATE ON media_items_fts_source
BEGIN
    INSERT INTO media_items_fts (media_items_fts, rowid, title, cast_names)
    VALUES ('delete', old.rowid, old.title, old.cast_names);

    INSERT INTO media_items_fts (rowid, title, cast_names)
    VALUES (new.rowid, new.title, new.cast_names);
END;

CREATE TRIGGER media_items_fts_source_ad
AFTER DELETE ON media_items_fts_source
BEGIN
    INSERT INTO media_items_fts (media_items_fts, rowid, title, cast_names)
    VALUES ('delete', old.rowid, old.title, old.cast_names);
END;

CREATE TRIGGER media_metadata_cache_fts_ai
AFTER INSERT ON media_metadata_cache
BEGIN
    INSERT INTO media_items_fts_source (rowid, media_item_id, title, cast_names)
    SELECT
        media_items.rowid,
        media_items.id,
        new.title,
        COALESCE((
            SELECT group_concat(media_metadata_cast_members.name, ' ')
            FROM media_metadata_cast_members
            WHERE media_metadata_cast_members.media_item_id = new.media_item_id
        ), '')
    FROM media_items
    WHERE media_items.id = new.media_item_id
    ON CONFLICT(media_item_id) DO UPDATE SET
        rowid = excluded.rowid,
        title = excluded.title,
        cast_names = excluded.cast_names;
END;

CREATE TRIGGER media_metadata_cache_fts_au
AFTER UPDATE ON media_metadata_cache
BEGIN
    DELETE FROM media_items_fts_source
    WHERE media_item_id = old.media_item_id
      AND old.media_item_id != new.media_item_id;

    INSERT INTO media_items_fts_source (rowid, media_item_id, title, cast_names)
    SELECT
        media_items.rowid,
        media_items.id,
        new.title,
        COALESCE((
            SELECT group_concat(media_metadata_cast_members.name, ' ')
            FROM media_metadata_cast_members
            WHERE media_metadata_cast_members.media_item_id = new.media_item_id
        ), '')
    FROM media_items
    WHERE media_items.id = new.media_item_id
    ON CONFLICT(media_item_id) DO UPDATE SET
        rowid = excluded.rowid,
        title = excluded.title,
        cast_names = excluded.cast_names;
END;

CREATE TRIGGER media_metadata_cache_fts_ad
AFTER DELETE ON media_metadata_cache
BEGIN
    DELETE FROM media_items_fts_source
    WHERE media_item_id = old.media_item_id;
END;

CREATE TRIGGER media_metadata_cast_members_fts_ai
AFTER INSERT ON media_metadata_cast_members
BEGIN
    INSERT INTO media_items_fts_source (rowid, media_item_id, title, cast_names)
    SELECT
        media_items.rowid,
        media_items.id,
        media_metadata_cache.title,
        COALESCE((
            SELECT group_concat(cast_members.name, ' ')
            FROM media_metadata_cast_members AS cast_members
            WHERE cast_members.media_item_id = new.media_item_id
        ), '')
    FROM media_items
    JOIN media_metadata_cache
        ON media_metadata_cache.media_item_id = media_items.id
    WHERE media_items.id = new.media_item_id
    ON CONFLICT(media_item_id) DO UPDATE SET
        rowid = excluded.rowid,
        title = excluded.title,
        cast_names = excluded.cast_names;
END;

CREATE TRIGGER media_metadata_cast_members_fts_au
AFTER UPDATE ON media_metadata_cast_members
BEGIN
    INSERT INTO media_items_fts_source (rowid, media_item_id, title, cast_names)
    SELECT
        media_items.rowid,
        media_items.id,
        media_metadata_cache.title,
        COALESCE((
            SELECT group_concat(cast_members.name, ' ')
            FROM media_metadata_cast_members AS cast_members
            WHERE cast_members.media_item_id = old.media_item_id
        ), '')
    FROM media_items
    JOIN media_metadata_cache
        ON media_metadata_cache.media_item_id = media_items.id
    WHERE media_items.id = old.media_item_id
    ON CONFLICT(media_item_id) DO UPDATE SET
        rowid = excluded.rowid,
        title = excluded.title,
        cast_names = excluded.cast_names;

    INSERT INTO media_items_fts_source (rowid, media_item_id, title, cast_names)
    SELECT
        media_items.rowid,
        media_items.id,
        media_metadata_cache.title,
        COALESCE((
            SELECT group_concat(cast_members.name, ' ')
            FROM media_metadata_cast_members AS cast_members
            WHERE cast_members.media_item_id = new.media_item_id
        ), '')
    FROM media_items
    JOIN media_metadata_cache
        ON media_metadata_cache.media_item_id = media_items.id
    WHERE media_items.id = new.media_item_id
    ON CONFLICT(media_item_id) DO UPDATE SET
        rowid = excluded.rowid,
        title = excluded.title,
        cast_names = excluded.cast_names;
END;

CREATE TRIGGER media_metadata_cast_members_fts_ad
AFTER DELETE ON media_metadata_cast_members
BEGIN
    INSERT INTO media_items_fts_source (rowid, media_item_id, title, cast_names)
    SELECT
        media_items.rowid,
        media_items.id,
        media_metadata_cache.title,
        COALESCE((
            SELECT group_concat(cast_members.name, ' ')
            FROM media_metadata_cast_members AS cast_members
            WHERE cast_members.media_item_id = old.media_item_id
        ), '')
    FROM media_items
    JOIN media_metadata_cache
        ON media_metadata_cache.media_item_id = media_items.id
    WHERE media_items.id = old.media_item_id
    ON CONFLICT(media_item_id) DO UPDATE SET
        rowid = excluded.rowid,
        title = excluded.title,
        cast_names = excluded.cast_names;
END;

CREATE TRIGGER media_items_fts_ad
AFTER DELETE ON media_items
BEGIN
    DELETE FROM media_items_fts_source
    WHERE media_item_id = old.id;
END;

INSERT INTO media_items_fts_source (rowid, media_item_id, title, cast_names)
SELECT
    media_items.rowid,
    media_items.id,
    media_metadata_cache.title,
    COALESCE((
        SELECT group_concat(media_metadata_cast_members.name, ' ')
        FROM media_metadata_cast_members
        WHERE media_metadata_cast_members.media_item_id = media_items.id
    ), '')
FROM media_items
JOIN media_metadata_cache
    ON media_metadata_cache.media_item_id = media_items.id;
