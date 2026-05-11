# Library Scan and Reconciliation

Linear issue: F-260

## Context

Kino has a canonical storage layout scanner from F-261, but Phase 1 still needs
a reconciliation surface that compares disk state with the catalog. The scan
must report drift for operators and future repair workflows, without mutating
the library automatically.

## Scope

`kino-library` owns the scan report:

- Walk the canonical library layout through `StorageLayoutScanner`.
- Read persisted `source_files` rows from SQLite.
- Report canonical files on disk with no matching `source_files.path` as
  orphans.
- Report `source_files` rows whose path no longer exists as missing files.
- Preserve layout violations from the storage layout scan.

The report is JSON-serializable and includes stable fields for a later "fix it"
workflow: source-file ids, media-item ids, paths, media path kind, and violation
kind.

## Interface

Phase 1 exposes the scan as an admin endpoint:

```text
GET /api/admin/library/scan
```

The endpoint returns the `LibraryScanReport` shape directly. It does not repair,
delete, move, or insert rows.

## Schema

Migration `0012_source_files.sql` adds the minimal durable source-file table
needed for reconciliation:

- `id`
- `media_item_id`
- `path`
- `created_at`
- `updated_at`

`path` is unique because a single canonical source file should map to one
catalog row.
