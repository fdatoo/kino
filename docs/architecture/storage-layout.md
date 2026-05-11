# Storage Layout

Kino owns the configured `library_root`.

Pointing Kino at a directory is a one-way migration decision. Files accepted by
ingestion are placed into Kino's canonical layout, and later scans evaluate the
directory against that same layout. Kino does not try to coexist with
hand-curated directory structures, mixed naming schemes, or another media
manager's library rules inside `library_root`.

## Policy

- `library_root` is managed storage. Existing entries may be moved into the
  canonical layout, ignored only when they are under a Kino-owned sidecar root,
  or reported as layout violations.
- Import sources are outside the library contract until ingestion accepts a
  file. After acceptance, import is one-way: the canonical path is the stable
  library path.
- Layout rules apply uniformly to writer and scan code. A path the writer would
  not produce is not treated as canonical by scans.
- Kino refuses to overwrite an existing canonical media path.

## Media Paths

Movie files live at:

```text
Movies/Title (Year)/Title (Year).ext
```

TV episode files live at:

```text
TV/Show/Season 02/Show - S02E03.ext
```

Season and episode numbers are zero-padded to at least two digits. Larger values
are not truncated. The source file extension is preserved.

Generated path segments are normalized by trimming whitespace, replacing path
separators and control characters with spaces, and collapsing repeated
whitespace. A segment that becomes empty is rejected.

## Owned Sidecars

`Metadata/` is reserved for Kino-managed metadata and other sidecars. Media
scans treat it as owned storage but do not discover playable media from it.

## Reconciliation

Library scan compares canonical files on disk with `source_files.path` rows in
the database. The scan is report-only in Phase 1:

- Orphans are canonical media files on disk with no matching source-file row.
- Missing files are source-file rows whose path no longer exists.
- Layout violations are filesystem entries that do not follow the canonical
  storage layout.

Kino does not auto-delete, move, relink, or create rows during scan.

## Startup Warning

If `library_root` is non-empty at startup, Kino logs a warning:

```text
this directory will be owned by Kino; existing contents will be treated as Kino-managed storage
```

The warning is intentional even when the directory already contains a prior Kino
library. It makes the ownership boundary explicit before scan or ingestion work
touches the directory.
