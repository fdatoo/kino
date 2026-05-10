# Provider Selection Logic

Linear: F-242

## Goal

Given a resolved request that needs fulfillment, choose the best configured
provider deterministically and produce a fulfillment-plan decision that can be
persisted through the existing plan history.

## Scope

- Add a small provider-selection model in `kino-fulfillment`.
- Accept configured provider descriptors from the caller rather than reading
  process config directly. The config model is a later provider-interface issue.
- Rank matching providers by declared capability and user preference.
- Support fallback by excluding providers that already failed to satisfy the
  request.
- Record `needs_provider` when a provider is selected and `needs_user_input`
  when none match.

## Ranking

Provider selection is deterministic:

1. Providers with no matching capability are excluded.
2. Providers listed as already rejected are excluded.
3. The best capability match wins. A media-kind-specific capability outranks a
   generic media capability.
4. Higher user preference wins among equal capability matches.
5. Provider id sorts ascending as the final tiebreaker.

The selector returns the full ranked list after exclusions. The first row is the
selected provider. If the list is empty, the plan decision is
`needs_user_input`.

## Persistence

The existing `request_fulfillment_plans` table stores only the top-level
decision and summary. This issue records the selected provider in the summary
and returns structured selection details from the in-process API. Adding
structured provider-attempt history belongs with the provider lifecycle work.
