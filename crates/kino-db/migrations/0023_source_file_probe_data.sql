ALTER TABLE subtitle_sidecars
ADD COLUMN forced INTEGER NOT NULL DEFAULT 0 CHECK (forced IN (0, 1));

CREATE TABLE source_file_probes (
    source_file_id TEXT PRIMARY KEY NOT NULL REFERENCES source_files(id) ON DELETE CASCADE,
    container TEXT,
    video_codec TEXT,
    video_width INTEGER CHECK (video_width IS NULL OR video_width > 0),
    video_height INTEGER CHECK (video_height IS NULL OR video_height > 0),
    video_hdr TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE source_file_audio_tracks (
    source_file_id TEXT NOT NULL REFERENCES source_files(id) ON DELETE CASCADE,
    track_index INTEGER NOT NULL CHECK (track_index >= 0),
    codec TEXT,
    language TEXT,
    channels INTEGER CHECK (channels IS NULL OR channels > 0),
    PRIMARY KEY (source_file_id, track_index)
);

CREATE TABLE source_file_subtitle_tracks (
    source_file_id TEXT NOT NULL REFERENCES source_files(id) ON DELETE CASCADE,
    track_index INTEGER NOT NULL CHECK (track_index >= 0),
    format TEXT NOT NULL CHECK (format IN ('srt', 'ass', 'json')),
    provenance TEXT NOT NULL CHECK (provenance IN ('text', 'ocr')),
    language TEXT NOT NULL CHECK (language != ''),
    forced INTEGER NOT NULL DEFAULT 0 CHECK (forced IN (0, 1)),
    PRIMARY KEY (source_file_id, track_index, format, provenance, language)
);
