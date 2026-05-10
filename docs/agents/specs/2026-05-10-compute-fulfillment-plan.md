# Compute Fulfillment Plan

Linear: F-240

## Goal

Centralize fulfillment planning in a deterministic function. Given a resolved
request, current library state for that identity, configured providers, and any
rejected providers, the planner returns one of:

- `already_satisfied`
- `needs_provider(provider, args)`
- `needs_user_input(reason)`

## Scope

- Add a pure planner in `kino-fulfillment`.
- Preserve provider ranking from `F-242`.
- Preserve library short-circuit behavior from `F-241`.
- Keep database reads and plan persistence in `RequestService`.

## Behavior

Planning first requires a canonical identity on the request. It then checks
library state before validating or ranking providers. This keeps an already
satisfied request from failing because of unrelated provider configuration.

If the library does not contain the identity, provider selection ranks matching
providers deterministically. The selected provider receives canonical identity
arguments. If no provider remains, the planner returns a typed user-input
reason.

## Test Plan

Unit tests cover each outcome: already satisfied, needs provider, no matching
provider, no remaining provider after rejection, and unresolved request error.
