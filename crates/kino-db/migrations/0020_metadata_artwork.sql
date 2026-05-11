ALTER TABLE media_metadata_cache
ADD COLUMN poster_local_path TEXT;

ALTER TABLE media_metadata_cache
ADD COLUMN backdrop_local_path TEXT;

ALTER TABLE media_metadata_cache
ADD COLUMN logo_local_path TEXT;

UPDATE media_metadata_cache
SET
    poster_local_path = poster_path,
    backdrop_local_path = backdrop_path,
    logo_local_path = logo_path
WHERE poster_local_path IS NULL;
