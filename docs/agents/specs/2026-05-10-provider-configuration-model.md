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
[providers.watch_folder]
path = "/srv/kino/incoming"
preference = 0
```

`ProvidersConfig` owns optional provider sections. `WatchFolderProviderConfig`
is a serde-derived typed config struct with provider-specific fields plus the
selection preference used by fulfillment ranking.

Missing provider sections are valid and mean no provider of that type is
configured. Present provider sections are validated during `Config::load`.

## Validation

`providers.watch_folder.path` is required when the section is present. Startup
rejects an empty path, a missing path, or a path that does not point to a
directory. This keeps provider misconfiguration in the same fail-fast path as
database, library, and TMDB settings.

## Extension

Adding a new first-party provider should add:

- a serde-derived provider config struct in `kino-core`;
- an optional field on `ProvidersConfig`;
- validation for that provider's required paths and scalar limits;
- a reference section in `kino.toml.example`.
