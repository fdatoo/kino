# ADR-0005 — mDNS responder crate choice

**Status:** Accepted
**Date:** 2026-05-15

## Context

Phase 4 adds native Apple clients that need to discover Kino servers on the LAN
without manual URL entry. The server needs to advertise `_kino._tcp` with TXT
metadata, run on macOS and Linux developer hosts, and avoid extra system setup
in CI.

Rust mDNS options are uneven. `mdns-sd` is a pure Rust responder/browser with a
channel-based API. `zeroconf` wraps platform C libraries such as Avahi or
dns-sd, which would add package and linker requirements. Older `mdns` crates
are oriented more toward browsing or are not maintained enough to own Kino's
server advertisement lifecycle.

## Decision

Use `mdns-sd` for the server responder.

It keeps discovery as a Cargo dependency, works without Avahi or Apple's
dns-sd service APIs, and gives Kino enough control to register, unregister, and
test `_kino._tcp.local.` advertisements directly.

## Consequences

Kino does not require system mDNS development packages on Linux or macOS. The
server owns the service registration lifecycle and logs discovery failures as
best-effort startup work rather than blocking the HTTP API.

Real mDNS tests still depend on multicast support from the host network stack.
The integration-style probe therefore stays behind a cargo feature while normal
CI exercises the construction and configuration paths.
