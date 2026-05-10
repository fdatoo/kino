# Re-resolution

Linear: F-238

## Goal

Re-resolution is an explicit request action, not a side effect of writing the
request projection. Every canonical identity assignment is versioned with
provenance, and the current `requests.canonical_identity_id` column remains only
the read-optimized latest projection.

## Design

Persist identity history in `request_identity_versions`:

- `(request_id, version)` is the primary key.
- `version` starts at `1` and increments per request.
- `canonical_identity_id` is the selected canonical media identity.
- `provenance` records why the identity was selected: match scoring or manual
  admin action.
- `status_event_id` links new versions to the status event written by the same
  transition.
- actor columns mirror request status events so provenance can name the
  responsible principal.

The fulfillment service owns identity writes:

- automatic match scoring writes version `1` with `match_scoring` provenance.
- manual `resolve_identity` is valid only for pending or disambiguation states.
- manual `re_resolve_identity` is valid only for active post-resolution states
  accepted by `RequestTransition::ReResolve`.
- generic `transition(Resolve | ReResolve)` is rejected because it cannot record
  a canonical identity version.

The HTTP API exposes deliberate re-resolution through
`POST /api/requests/:id/re-resolution`. The response is the standard request
detail projection plus ordered identity versions.

## Non-goals

- Provider-specific TMDB/TVDB metadata is still outside this request model.
- Admin UI controls are not implemented here; the API action is the observable
  hook the UI can call.
