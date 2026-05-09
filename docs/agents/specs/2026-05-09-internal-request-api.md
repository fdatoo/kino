# Internal Request API

Linear: F-230

## Goal

Expose the Phase 1 request lifecycle over JSON HTTP for admin and curl-driven
use. There is no auth and no external client contract yet.

## Endpoints

- `POST /api/requests` creates a pending request and returns the detail
  projection. The request body is JSON with an optional `message`.
- `GET /api/requests/:id` returns the detail projection for one request.
- `GET /api/requests` returns the default projection for all requests.
- `DELETE /api/requests/:id` cancels an active request and returns the updated
  detail projection.

## Error behavior

- Malformed request ids return `400`.
- Unknown request ids return `404`.
- Invalid state transitions return `409`.
- Persistence and data-shape failures return `500` and are logged.

## Notes

The HTTP layer delegates state changes to `kino-fulfillment`; it does not own
request state-machine rules. List responses intentionally use the default
projection without status events, matching ADR-0002.
