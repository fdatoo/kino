# Watch Folder Provider

Linear issue: F-250

## Context

The full ingestion pipeline is still pending, so this issue implements the
provider-side handoff: a watch-folder job reports a stable source file through
`FulfillmentProviderJobStatus::Completed { source_paths }`. The future provider
orchestrator can use that source path list to move the request from
`fulfilling` to `ingesting`, matching the existing request state machine.

## Behavior

`WatchFolderProvider` watches its configured directory by polling during
provider `status` calls. For one active job it:

- returns `Queued` when the directory has no regular files;
- records the first regular file it sees and returns `Running`;
- resets the observation window when that file's size changes;
- returns `Completed { source_paths }` only after the same file has had the same
  size for the configured stability window;
- returns `Cancelled` after cancellation without deleting user-supplied files.

The default stability window is five seconds. Tests construct the provider with
a shorter window to keep the behavior deterministic and fast.

## Configuration

`providers.watch_folder.stability_seconds` is optional and defaults to five.
Startup rejects zero because a zero-second stability window would allow
partially written files to be picked up immediately.
