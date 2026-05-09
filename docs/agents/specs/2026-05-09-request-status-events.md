# Request Status Events

Linear: F-231

## Problem

Kino needs a durable audit trail for request state changes. The current `main`
branch has no request schema or internal request API, so this issue introduces
the smallest request surface needed to make status events meaningful: persisted
requests, validated state transitions, and a read model that returns the event
log with the current state.

## Design

Persist requests in `requests`:

- `id` is `kino_core::Id`.
- `state` is one of `pending`, `resolved`, `planning`, `fulfilling`,
  `ingesting`, `satisfied`, `failed`, or `cancelled`.
- `created_at` and `updated_at` are `kino_core::Timestamp`.
- `failure_reason` is nullable except when `state = failed`, where a typed
  reason is required.

Persist status events in `request_status_events`:

- `id` is `kino_core::Id`.
- `request_id` references `requests(id)`.
- `from_state` is nullable so request creation can be represented as the first
  event.
- `to_state` is required.
- `occurred_at` is `kino_core::Timestamp`.
- `message` is nullable.
- `actor_kind` is nullable or one of `system`, `user`.
- `actor_id` is nullable and only set for user actors.

The fulfillment API owns state-machine validation. Every create or transition is
one SQLite transaction that updates the request row and appends exactly one
event. The database rejects direct updates or deletes against status events so
the log stays append-only. Reads return `RequestDetail`, which contains the
current request projection and all events ordered by occurrence time.

## Non-goals

- Media identity persistence is left to the resolution issue.
- Fulfillment plans and provider attempts are left to the planning/provider
  issues.
- External server/client endpoints are left to the API issue.
