CREATE TABLE transcode_color_downgrades (
    transcode_output_id BLOB PRIMARY KEY
        REFERENCES transcode_outputs(id) ON DELETE CASCADE,
    kind                TEXT NOT NULL
        CHECK (kind IN ('dv_to_hdr10', 'hdr10_to_sdr', 'dv_to_sdr')),
    note                TEXT,
    created_at          INTEGER NOT NULL
);
