# Tracing

Kino uses `tracing` for diagnostics and operational visibility. Logs are events;
spans are the context that makes those events useful.

## Span Names

Name spans as `crate::module::operation`.

Use the Rust crate identifier, not the package name: `kino_core`, not
`kino-core`. Keep operation names concrete and verb-oriented:

- `kino::startup::run`
- `kino_db::pool::open`
- `kino_fulfillment::request::resolve`

Do not put dynamic data in span names. Values belong in fields.

## Request Id

The tracing field name is always `request_id`.

Use the same `request_id` on every span and event that belongs to one inbound
request. When Kino receives a caller-provided request id, validate it before
using it. When Kino creates one, use `kino_core::Id::new()` and format it as the
standard hyphenated UUID string.

Process-level spans that are not serving a request should declare
`request_id = tracing::field::Empty` when they demonstrate or reserve the
correlation field. Do not invent sentinel values such as `none`, `startup`, or
`unknown`.

## Fields

Attach fields to a span when the value is stable context for the operation:

- ids: `request_id`, `media_id`, `library_item_id`
- configuration that affects behavior: `server.listen`, `db.path`
- bounded categories: `provider`, `format`, `state`
- numeric inputs or results that are useful for debugging: `candidate_count`

Prefer fields on the narrowest span that owns the value. Avoid duplicating the
same field on every event when it is already present on the active span.

Fields must be safe to keep in production logs. Do not attach secrets, tokens,
full file contents, or unbounded user text.

## Events

Emit events for things that happen inside a span:

- lifecycle changes: started, ready, completed
- decisions: cache hit, provider selected, request already satisfied
- retries and degraded behavior
- errors, with the error attached as `error = %err` or `error = ?err`

Use `info` for important lifecycle and state transitions operators should see in
normal operation. Use `debug` for detailed decisions and intermediate values.
Use `warn` when Kino recovered but behavior was degraded or delayed. Use `error`
when an operation failed and the caller or supervisor must handle it.

## Async Propagation

Spans do not automatically follow work moved into a new task. Instrument async
boundaries explicitly:

```rust
use tracing::Instrument;

let span = tracing::info_span!(
    "kino_fulfillment::request::resolve",
    request_id = %request_id,
);

tokio::spawn(async move {
    resolve_request(request_id).await
}.instrument(span));
```

When sending work through a channel or queue, include `request_id` in the message
type and recreate a child span on the receiver side. Do not rely on task-local
state for cross-task request correlation.

The `kino` binary's startup path uses the same pattern even though startup has no
request id: it creates `kino::startup::run`, declares an empty `request_id`
field, and instruments the async startup boundary before emitting `ready`.
