# Request Data Model

Linear: F-227

## Goal

Define the durable request projection shared by core and persistence. A request
captures who asked for something, what raw target they asked for, current state,
timestamps, and nullable links to later resolution and planning records.

## Shape

`kino-core` owns the request value types:

- `Request` is the current projection.
- `RequestRequester` is `anonymous`, `system`, or a future `user` id.
- `RequestTarget` stores the raw query and an optional canonical identity id.
- `RequestState` and `RequestFailureReason` remain typed enums with stable
  persisted strings.

The database stores requester as `requester_kind` plus nullable `requester_id`
so multi-user support can add a users table later without rewriting request
rows. `canonical_identity_id` and `plan_id` are nullable ids for future tables;
they are intentionally not constrained until those tables exist.

## Persistence

Existing status-event work already created the request table and state
transition flow. This issue adds a forward migration for the missing model
columns and updates create, get, update, and list paths to hydrate the shared
core projection.
