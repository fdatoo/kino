# Plan Persistence + Observability

## Goal

Persist every computed fulfillment plan for a request and surface both the
current plan and historical plans through the internal request API.

## Scope

- Add an append-only `request_fulfillment_plans` table.
- Keep `requests.plan_id` as the current-plan pointer.
- Extend `RequestDetail` with `current_plan` and `plan_history`.
- Add a narrow internal endpoint for recording a newly computed plan.
- Treat every re-plan as a new row with a monotonic per-request version.

## Model

A plan records the request id, plan id, per-request version, decision, summary,
optional status event link, creation time, and actor. The initial decision set is
the Phase 1 planning contract:

- `already_satisfied`
- `needs_provider`
- `needs_user_input`

The summary is required so the stored row is auditable without inferring intent
from logs.

## API

`GET /api/requests/{id}` continues to return the request detail shape, now with:

- `current_plan`: the row referenced by `request.plan_id`, if one exists.
- `plan_history`: all plan rows for the request ordered by version.

`POST /api/requests/{id}/plans` records a new computed plan and returns the
updated request detail. The call is valid only while the request is in
`planning`; callers move to planning through the existing state transition.

## Notes

This issue does not implement library matching or provider ranking. Those
callers can later compute richer decisions and use this persistence surface
without rewriting request history semantics.
