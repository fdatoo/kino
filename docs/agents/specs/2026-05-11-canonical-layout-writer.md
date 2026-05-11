# Canonical Layout Writer

Linear issue: F-255

## Context

Ingestion needs a library-owned filesystem step after a source file has been
accepted and identified. This step places the source file under Kino's canonical
library root so downstream catalog, subtitles, and transcode handoff code can
refer to stable paths.

## Scope

`kino-library` owns the writer:

- Movies are placed at `Movies/Title (Year)/Title (Year).ext`.
- TV episodes are placed at `TV/Show/Season XX/Show - SXXEYY.ext`.
- The source file extension is preserved.
- Path segments are normalized to keep separators and control characters out of
  generated directories and filenames.

The writer supports two transfer modes:

- `hard_link`: default, preserves the original source path and creates the
  canonical path as another directory entry for the same file.
- `move`: renames the source file into the canonical path.

Cross-device hard-link failures, missing source paths, destination conflicts,
and unreadable filesystem state are returned to the caller.

## Configuration

`kino_core::Config` adds `[library].canonical_transfer`, defaulting to
`hard_link`. It can be overridden with `KINO_LIBRARY__CANONICAL_TRANSFER`.
