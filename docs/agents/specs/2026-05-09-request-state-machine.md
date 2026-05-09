# Request State Machine

Linear: F-229

## Goal

Encode request lifecycle rules in `kino-fulfillment` so callers use typed
transition commands instead of writing states directly.

## Rules

The normal forward path is:

`pending -> resolved -> planning -> fulfilling -> ingesting -> satisfied`

`failed` and `cancelled` are terminal outcomes allowed from any active
non-terminal state. Terminal states reject all later transitions.

`re-resolve` is the only backward transition. It moves a request from
`planning`, `fulfilling`, or `ingesting` back to `resolved` so the pipeline can
restart from a new canonical resolution without reopening terminal requests.

## Validation

`RequestTransition` owns command-level legality because `resolve` and
`re-resolve` both target `resolved` but are valid from different source states.
Invalid commands return `Error::InvalidTransition` with the source state,
transition command, and target state.
