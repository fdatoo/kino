# File Probe Identification

Linear issue: F-252

## Context

The ingestion pipeline needs a typed description of provider-produced media
files before request verification, metadata work, subtitle indexing, canonical
placement, and transcode handoff can make decisions. `ffprobe` already exposes
the needed container and stream facts, so Kino should wrap that tool with a
small typed boundary rather than passing raw JSON through the rest of the
pipeline.

## Scope

`kino-fulfillment::probe` owns the Phase 1 probing boundary:

- `FfprobeFileProbe` validates the source path, runs an ffprobe-compatible
  executable, and parses JSON from `-show_format -show_streams`.
- `ProbeResult` captures source path, container names, title, duration, video
  streams, audio streams, and embedded subtitle streams.
- `ProbeSubtitleKind` classifies SRT and ASS as text subtitles, PGS and VOBSUB
  as image subtitles, and leaves unknown codecs explicit as `Other`.
- `ProbeResult::as_probed_file` projects the rich probe result into the lighter
  `ProbedFile` structure used by F-253 request matching.

Probe errors are typed. Filesystem validation, process launch failures,
non-zero ffprobe exits, invalid JSON, and invalid duration values each return a
specific `ProbeError` instead of panicking.

The unit tests keep process behavior deterministic with an injected
ffprobe-compatible script. A separate integration test probes a committed tiny
MKV fixture with the real `ffprobe`; CI installs `ffmpeg` so this path runs in
the required test suite.
