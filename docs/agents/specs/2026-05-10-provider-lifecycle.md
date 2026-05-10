# Provider Lifecycle

Linear: F-247

## Goal

Define the provider lifecycle contract used by fulfillment:

- `start` accepts provider arguments and returns a provider-owned job handle.
- `status` reports queued, running progress, completed, cancelled, or failed.
- `cancel` aborts provider work and cleans up provider-owned partial state.

## Contract

`FulfillmentProvider` is intentionally narrow. It exposes stable provider id,
declared capabilities, and the three lifecycle methods. Methods return boxed
futures so providers can do async work without requiring another trait macro.

Job handles are provider-scoped. Kino stores and passes the handle back to the
same provider; it does not interpret the job id.

## Cancellation

Cancellation must not leak partial provider state. `cancel` returns
`FulfillmentProviderCancelResult` with cleanup status:

- `CleanedUp` means partial files, temp dirs, and in-progress provider state
  were removed before the method returned.
- `NothingToCleanUp` means the provider had no partial state for that job.

If cleanup fails, the provider returns a `FulfillmentProviderError` instead of a
successful cancellation result.

## Notes

This issue defines the in-process lifecycle contract only. Provider attempt
persistence and durable job scheduling remain future lifecycle orchestration
work.
