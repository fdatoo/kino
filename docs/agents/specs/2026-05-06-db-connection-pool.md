# DB connection pool - Design Spec

**Linear:** F-201 (parent: F-188 kino-db harness)
**Phase:** 0 - Foundations
**Date:** 2026-05-06

## Done when

- `Db::open(&Config) -> Result<Db>` opens the configured SQLite database.
- WAL mode is enabled before reader connections are established.
- The applied SQLite pragmas are documented in `kino-db`.
- Tests prove the live connection settings match the documented defaults.

## Design

Kino remains a single-binary SQLite deployment, but the database wrapper should
still encode the intended concurrency model instead of leaving it to call sites.
`kino-db` exposes a `Db` value with two sqlx pools:

- A writer pool with one connection. It creates the database file if needed and
  applies `journal_mode = WAL`.
- A read-only reader pool with eight connections. It opens after the writer so
  the database is already in WAL mode.

Common connection options are applied to both pools:

- `busy_timeout = 5s` to wait through short writer contention.
- `synchronous = NORMAL`, the usual durability/throughput tradeoff for WAL.
- `foreign_keys = ON` so constraints are enforced consistently.
- `PRAGMA optimize` on close, using sqlx's `optimize_on_close` hook.

The wrapper deliberately exposes `read_pool()` and `write_pool()` rather than a
generic pool accessor. That makes accidental writes through the reader path fail
at SQLite's read-only boundary and gives later migration code an obvious writer
entry point.
