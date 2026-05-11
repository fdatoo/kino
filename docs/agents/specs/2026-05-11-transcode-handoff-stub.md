# Transcode Handoff Stub

Linear issue: F-257

## Context

Phase 1 needs a stable ingestion-to-transcode seam before the real FFmpeg job
pipeline exists. The full transcode job model, queue, output policy, and
hardware dispatch remain Phase 3 work.

## Interface

`kino-transcode` owns the handoff contract:

- `SourceFile` identifies the source file ingestion is ready to hand off.
- `TranscodeHandOff` accepts a `SourceFile` and returns a `TranscodeReceipt`.
- `NoopTranscodeHandOff` implements the trait by recording a receipt with
  message `would transcode source file` and doing no media work.

The trait returns boxed futures, matching the existing provider lifecycle style
without adding a new async-trait dependency.

## Ingestion Use

`kino-fulfillment` adds `IngestionPipeline`, a minimal source-file ingestion
entry point. `ingest_source_file` creates the transcode `SourceFile`, calls the
configured `TranscodeHandOff`, and returns both the source-file projection and
the transcode receipt. This is the current executable seam until durable
`SourceFile` persistence lands.
