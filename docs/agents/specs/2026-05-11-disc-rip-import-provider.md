# Disc Rip Import Provider

Linear issue: F-249

## Context

The ingestion pipeline is still represented by the provider handoff. For disc
rips, that handoff must support more than one file because episode-pack rips can
produce multiple titles from one MakeMKV output directory.

## Behavior

Add `DiscRipProvider` in `kino-fulfillment`. It scans its configured directory
recursively when a job starts and records all supported media files as completed
source paths. Supported files are common disc-rip video containers: `mkv`,
`m4v`, `mp4`, `m2ts`, and `ts`.

The provider:

- declares `AnyMedia`;
- rejects a configured rip directory with no supported media files using a
  permanent provider error;
- validates each selected file can be opened before accepting the job;
- returns `Completed { source_paths }` with one path for movie-style rips and
  multiple sorted paths for episode-pack rips;
- returns `NothingToCleanUp` on cancellation because these are user-supplied
  source files, not provider-owned temporary files.

## Configuration

Add optional config:

```toml
[providers.disc_rip]
path = "/srv/kino/rips"
preference = 0
```

Startup validates that `path` is present, non-empty, and points to a directory.
