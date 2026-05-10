# Manual Import Provider

Linear issue: F-251

## Context

Manual import is an admin override for cases where automatic providers do not
notice or cannot produce a file. The current ingestion and catalog crates do not
yet have file probing or canonical layout writing, so this issue should stop at
the stable handoff point already modeled by the request state machine:
`fulfilling -> ingesting`.

## Provider

Add `ManualImportProvider` in `kino-fulfillment`. It uses the existing
`FulfillmentProvider` lifecycle and requires `FulfillmentProviderArgs` to carry
a source path for this provider. The provider:

- declares `AnyMedia`;
- rejects missing source paths, non-existent paths, directories, and unreadable
  files with permanent provider errors;
- records accepted imports as provider jobs whose status is `Completed`;
- returns `NothingToCleanUp` on cancellation because it does not create
  provider-owned temporary state.

The provider does not participate in automatic provider selection yet. It is
admin-invoked with a concrete request and concrete file path.

## Admin API

Add `POST /api/admin/requests/{id}/manual-import` with JSON:

```json
{
  "path": "/absolute/or/relative/file/path",
  "message": "optional operator context"
}
```

The endpoint reads the request, verifies it can enter `ingesting`, starts the
manual provider, then records `StartIngesting` with a status-event message that
includes the accepted path and provider job id. Provider path errors surface to
the caller as `400 Bad Request`; invalid request state remains a conflict.

When the ingestion pipeline lands, it can consume this same state transition and
provider job handoff instead of changing the admin contract.
