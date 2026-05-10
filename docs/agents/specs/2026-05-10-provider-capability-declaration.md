# Provider Capability Declaration

Linear issue: F-246

## Context

Provider selection already accepts configured provider descriptors and ranks matching
providers by capability specificity, preference, and provider id. The provider
lifecycle trait also exists, but capability declarations were still plain slices at
the descriptor boundary.

## Decision

Introduce `FulfillmentProviderCapabilities` as the typed declaration a provider
implementation returns from `FulfillmentProvider::capabilities`.

The type is a small borrowed wrapper around `FulfillmentProviderCapability`
entries. This keeps provider implementations simple while giving selection one
stable API for checking whether a provider can satisfy a request shape.

Selection continues to reject invalid descriptors before planning, including empty
capability declarations. It filters candidates by the best capability match for
the resolved canonical identity kind. Providers whose declarations do not match
the request are not ranked and cannot be selected.

## Non-goals

This does not define external plugin loading. Future command or webhook provider
adapters can implement `FulfillmentProvider` and expose their remote capabilities
through the same typed declaration.
