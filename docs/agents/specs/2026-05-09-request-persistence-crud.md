# Request Persistence CRUD

Linear: F-228

## Goal

Expose the request persistence operations needed by later fulfillment work:
create, get, list, and update via state transition plus transition metadata.

## List Query

Request listing uses offset pagination. The query shape is:

- optional `state` filter
- `limit`, default `50`, maximum `250`
- `offset`, default `0`

Offset pagination is enough for the Phase 1 internal API because request volume
is expected to be small and operators need predictable page jumps more than a
stable long-lived cursor. Results are ordered by `(created_at, id)` so ties are
deterministic. Responses include `next_offset` only when another page may exist.

## Update Semantics

Updates go through the request state machine. A transition updates the current
request row and accepts optional metadata (`actor`, `message`) that is recorded
with the status event in the same transaction.
