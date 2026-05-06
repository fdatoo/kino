# Configure tracing subscriber

## Goal

Implement `kino_core::tracing::init(&Config)` for Linear issue F-204.

## Behavior

- `Config` owns `log_format`, loaded from `log_format` in TOML or `KINO_LOG_FORMAT`.
- Supported formats are `pretty` and `json`; the default is `pretty`.
- `init(&Config)` installs one global `tracing-subscriber` subscriber.
- Log level filter precedence is:
  1. `RUST_LOG`
  2. `KINO_LOG`
  3. `Config::log_level`
- Invalid format values and invalid filter expressions return typed errors.
- `KINO_LOG` is a runtime logging control, not a config schema field, so the
  config loader ignores it while still accepting `KINO_LOG_FORMAT`.

## Test Plan

- Unit-test config defaults and `KINO_LOG_FORMAT`.
- Unit-test rendering for both formats with a local subscriber and captured
  writer:
  - pretty output is valid UTF-8 and includes the event
  - JSON output parses as JSON and includes the event
