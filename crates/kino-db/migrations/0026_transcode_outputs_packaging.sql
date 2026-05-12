ALTER TABLE transcode_outputs ADD COLUMN directory_path TEXT;
ALTER TABLE transcode_outputs ADD COLUMN playlist_filename TEXT;
ALTER TABLE transcode_outputs ADD COLUMN init_filename TEXT;
ALTER TABLE transcode_outputs ADD COLUMN encode_metadata_json TEXT;
