# Probed File Request Match

Linear issue: F-253

## Context

The file-probe implementation is still a separate ingestion slice, but the
request pipeline needs the verification contract now: after a provider returns a
candidate file and the probe has extracted basic facts, Kino must reject obvious
wrong-file results instead of silently accepting them.

## Scope

`kino-fulfillment` owns the matcher:

- `ExpectedProbedFile` carries the request-side facts selected during
  resolution/enrichment: canonical identity, expected title, expected runtime,
  and any required audio/subtitle languages.
- `ProbedFile` carries the probe output: detected title, duration, and language
  tracks.
- `match_probed_file` is a pure matcher that reports wrong title, missing title,
  missing duration, duration mismatch, and missing language tracks.

Runtime matching accepts files within the greater of 20% or 300 seconds of the
expected duration. That is deliberately loose until F-252 lands real container
probing and metadata enrichment can provide tighter media-kind-specific policy.

## Request State

`RequestService::verify_probed_file` applies the matcher to an `ingesting`
request. A match leaves the request in `ingesting` so the rest of ingestion can
continue. A mismatch transitions the request to `failed` with
`RequestFailureReason::IngestFailed` and records a status-event message listing
the mismatches.
