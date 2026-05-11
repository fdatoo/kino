# Provider Configuration Model

Linear issue: F-245

## Context

Provider selection accepts configured provider descriptors, but startup config
did not yet have a typed provider section. Provider configuration belongs in the
single `kino_core::Config` type so malformed config fails before the process
starts serving.

## Decision

Add a top-level `providers` section to `kino.toml` with one typed subsection per
known provider. The initial model includes:

```toml
[providers.disc_rip]
path = "/srv/kino/rips"
preference = 0

[providers.watch_folder]
path = "/srv/kino/incoming"
preference = 0
stability_seconds = 5
```

`ProvidersConfig` owns optional provider sections. `DiscRipProviderConfig` and
`WatchFolderProviderConfig` are serde-derived typed config structs with
provider-specific fields and the selection preference used by fulfillment
ranking. The watch-folder config also owns the file-stability window used before
handing a file to ingestion.

Missing provider sections are valid and mean no provider of that type is
configured. Present provider sections are validated during `Config::load`.

## Validation

Provider `path` fields are required when their sections are present. Startup
rejects an empty path, a missing path, or a path that does not point to a
directory. `providers.watch_folder.stability_seconds` defaults to five and must
be positive. This keeps provider misconfiguration in the same fail-fast path as
database, library, and TMDB settings.

## Extension

Adding a new first-party provider should add:

- a serde-derived provider config struct in `kino-core`;
- an optional field on `ProvidersConfig`;
- validation for that provider's required paths and scalar limits;
- a reference section in `kino.toml.example`.
