# Provider Error Model

Linear: F-248

## Goal

Provider failures must tell Kino whether retry is useful. A provider returns a
typed error:

- `transient`: retry policy applies.
- `permanent`: fail the request immediately.

## Retry Policy

The default retry policy allows three failures before the request is failed. The
first transient failure retries after 30 seconds, then doubles with each
failure, capped at 300 seconds.

The policy is explicit in `ProviderRetryPolicy` so tests and future provider
lifecycle code can pass smaller windows without hidden process state. A failure
count of zero is invalid for scheduling and returns no retry.

## Request Handling

`RequestService::handle_provider_error` applies the model:

- transient error with retry budget remaining returns a retry decision and does
  not mutate request state;
- transient error with exhausted retry budget transitions the request to
  `failed`;
- permanent error transitions the request to `failed`.

Failed provider errors use `RequestFailureReason::AcquisitionFailed`. The
provider id, error class, code, and message are attached to the failure status
event message so operators can see the concrete provider failure in request
history.

## Notes

There is no provider attempt table yet. This issue defines the reusable error
and retry contract plus the request-state behavior that the future provider
lifecycle code should call.
