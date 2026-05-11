# Image Subtitle Frame Extraction

Linear issue: F-333

## Context

The OCR pipeline needs bitmap subtitle events as image files with start and end
timestamps. Text subtitle indexing already lives in `kino-library`, so this
slice adds a separate image-subtitle extraction surface there without changing
the text sidecar path.

## Scope

`kino-library::subtitle_image_extraction` owns:

- `ImageSubtitleFrame`, the OCR input record containing `start`, `end`, and the
  extracted image path.
- `ImageSubtitleExtraction`, a trait that lets OCR use either real ffmpeg output
  or synthetic fixtures in tests.
- `FfmpegImageSubtitleExtractor`, which stages output under
  `<subtitle_staging_dir>/<sha256(input_file_hash, stream_index)>`.

The ffmpeg command is:

```sh
ffmpeg -hide_banner -nostdin -y -i <input> -map 0:<stream_index> -f image2 -frame_pts true <track_dir>/frame-%020d.png
```

`stream_index` is the absolute stream index reported by ffprobe. The extractor
also invokes ffprobe with `-show_streams -show_packets` to attach packet start
and duration timestamps to extracted images.

## Fixtures

The repository currently has `crates/kino-fulfillment/tests/fixtures/probe_sample.mkv`,
which contains a text subtitle stream only. There is no tiny PGS, VOBSUB, or DVB
fixture, so this change covers the trait and content-addressing behavior with
unit tests and leaves a real ffmpeg round-trip test pending fixture binaries.
