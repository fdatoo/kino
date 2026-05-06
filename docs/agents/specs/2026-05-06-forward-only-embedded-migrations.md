# Forward-only embedded migrations

**Linear:** F-202 (parent: F-188 kino-db harness)
**Phase:** 0 - Foundations
**Date:** 2026-05-06

## Done when

- `kino-db/migrations/` contains forward-only SQL files named
  `0001_description.sql`, `0002_description.sql`, and so on.
- Migrations are embedded at compile time and `cargo` rebuilds `kino-db` when
  the migration directory changes.
- `Db::open` applies pending migrations on the writer pool before reader
  connections are opened.
- A `schema_migrations` table records applied versions, descriptions, checksums,
  and execution time.
- Startup fails with a clear `kino_db::Error` when a migration is missing,
  modified after application, out of order, or fails to execute.

## Design

Use `sqlx::migrate!` for compile-time migration discovery and embedding, but
run the resulting embedded migration list through Kino's own forward-only
SQLite runner. SQLx's built-in runner tracks state in `_sqlx_migrations`; F-202
requires `schema_migrations`, so `kino-db` owns the bookkeeping table.

The runner:

- creates `schema_migrations` if needed;
- rejects reversible `.up.sql` and `.down.sql` migration naming;
- validates already-applied checksums against the embedded migration list;
- rejects gaps where a migration older than the latest applied version has not
  been applied;
- applies each pending migration and its bookkeeping row in a single
  transaction.

`Db::open` keeps the startup order from F-201: open the writer with
`create_if_missing`, enable WAL, run migrations, then open read-only pools.
