# Fulfillment Provider Trait

Linear issue: F-244

## Context

Provider interface work now has typed capabilities, lifecycle methods,
cancellation semantics, provider errors, retry policy, and config. The
remaining F-244 requirement is to make the trait decision explicit and provide
the first in-tree implementation.

## Trait Location

`FulfillmentProvider` lives in `kino-fulfillment`, not `kino-core`.

The trait depends on fulfillment-specific types: provider capabilities, provider
arguments, job handles, job status, cancellation results, and provider errors.
Keeping it in `kino-fulfillment` avoids making `kino-core` own orchestration
concepts that only the fulfillment pipeline uses. `kino-core` remains the shared
home for process config, ids, time, request primitives, and canonical identity
types.

## First-Party Provider

`WatchFolderProvider` is the first in-tree implementation. It is constructed
from the typed `WatchFolderProviderConfig`, exposes a planning descriptor, and
implements the provider lifecycle:

- declares `AnyMedia` capability;
- accepts a fulfillment job and records it as queued;
- reports queued or cancelled status;
- cancels queued work with `NothingToCleanUp`.

Actual filesystem watching and file-stability detection belong to the dedicated
watch-folder provider issue. This implementation establishes the trait boundary
and a concrete provider shape without introducing hidden background state.
