# Kino — Vision

> A purpose-built, end-to-end self-hosted media platform written in Rust.

## 1. Why Kino exists

The modern self-hosted media stack works, but it is a patchwork. A typical setup runs separate tools for library management, file ingestion, transcoding, and playback — each competent, none designed with the others in mind.

The cost of that patchwork shows up in concrete ways:

- **Integration is glue, not architecture.** State lives in several different SQLite databases. A movie's "status" is reconstructed by polling APIs across services. Failures cascade in ways nobody owns end-to-end.
- **Each tool carries baggage Kino doesn't need.** Library managers support a dozen ingestion sources Kino will never use. Jellyfin ships a web UI, a plugin system, and live TV/DVR features that exist for users Kino isn't trying to serve. General-purpose transcoding tools are slow and fiddly for the one pipeline a home user actually runs.
- **Resource use is wasteful.** Half a dozen JVM, .NET, and Python processes sit idle most of the day. Inter-service traffic that should be a function call is HTTP-with-JSON.
- **No single point of truth.** Want to know why a file isn't showing up in the library? Check three different services' logs. The answer is somewhere in there.

Kino is the bet that a single, opinionated, purpose-built stack — owned end-to-end and written in one language with one data model — is dramatically better than the patchwork it replaces. Not a superset of features, but a deliberate, narrower system that does the actual job well.

## 2. What Kino is

Kino is a self-hosted platform for managing and watching a personal media library — content the user owns: ripped discs, home videos, personal recordings, files purchased or otherwise legitimately acquired. It handles the full lifecycle:

```
Ingest  →  Catalog  →  Transcode  →  Serve  →  Play
```

One server process. One source of truth. Native Apple clients on the consumption side.

Kino is also a request-driven system. Users can express intent ("I want to watch this") from client devices, and Kino resolves that request into a concrete media item within the library. Importantly, Kino does not assume where media comes from — it orchestrates fulfillment using user-configured inputs and adapters rather than embedding acquisition sources directly.

It is built in Rust as a **modular monolith**: a single repository of separable crates that compose into one deployable binary. The pieces are independently designed and testable, but they share types, share a database, and ship together.

## 3. Goals and non-goals

### Goals

- **One binary, one config, one database.** Setup should be downloading a binary, pointing it at a media directory, and going.
- **Vertically integrated by design.** Ingestion, cataloging, transcoding, and playback are first-class citizens of the same system, not bolted-together services.
- **Excellent on Apple platforms.** Native iOS, macOS, and tvOS clients are part of the project, not afterthoughts. Direct play wherever possible, sane transcode fallback when not.
- **Quality-aware transcoding.** Smart, per-title encoding that targets a quality level (e.g. VMAF), not a bitrate. Hardware-accelerated by default.
- **Request-driven workflow.** Users can request content from client devices; Kino tracks, resolves, and fulfills those requests into the library.
- **Clear separation of orchestration and acquisition.** Kino manages what should exist in the library and when; users control where it comes from.
- **Honest about scope.** A real Jellyfin alternative for a serious home library — multi-TB, 4K HDR, multiple concurrent streams — but not for everyone, and not a feature-for-feature port.

### Non-goals

- **Live TV and DVR.** Out of scope, possibly forever.
- **Music and audiobooks** (initially). Movies and TV first.
- **Bundled acquisition sources.** Kino does not ship with built-in indexers, trackers, or content sources. Any external acquisition is user-configured.
- **"Download anything" UX.** Kino is not designed or marketed as a tool for acquiring arbitrary media without regard to ownership or licensing.
- **Plugin ecosystem.** Kino is opinionated. Extensibility lives at well-defined edges (provider adapters, metadata providers), not throughout the system.
- **Windows and Android clients** (initially). Apple platforms first. The server is Linux-first, macOS-supported for development.
- **Web UI as the primary surface.** A minimal admin web UI exists for setup and operations. Day-to-day use happens in native clients.

## 4. Architecture

### Modular monolith

One repository, one binary, multiple crates. The crates have clean boundaries and could in principle be split into services later — but they won't be, because the whole point is to avoid the integration tax that comes with that.

Rough crate layout:

- `kino-core` — shared types, errors, configuration, the central data model.
- `kino-db` — schema, migrations, queries. SQLite for v1; Postgres possible later.
- `kino-fulfillment` — the request-to-library pipeline: request tracking, resolution against canonical media identity, the provider adapter interface, and the first-party providers (disc rip, watch folder, manual import).
- `kino-library` — on-disk layout, metadata enrichment (TMDB/TVDB), the canonical catalog.
- `kino-transcode` — the encoding pipeline: probing, decision logic, FFmpeg orchestration, hardware acceleration.
- `kino-server` — HTTP/gRPC API, auth, streaming endpoints, session management.
- `kino-admin` — minimal web UI for configuration and operations.
- `kino-cli` — operational tooling.

Apple clients live in a separate repository (Swift, not Rust) and consume the server API.

### Data model

A single SQLite database, owned by the server. Every other crate reads and writes through `kino-db`. No separate state stores, no service-to-service syncing. The schema models the lifecycle directly: a `Request` resolves to a `MediaItem`, which has one or more `SourceFiles` (the originals as ingested), each of which has zero or more `TranscodeOutputs` (what clients actually stream). Status questions become single queries, not multi-service investigations.

### API surface

A single API serves both the admin UI and the native clients. gRPC internally where it's a fit, HTTP/JSON at the edges where ecosystem support matters (clients, browsers, scripts).

Streaming uses HLS with byte-range fallback for direct play. Authentication is token-based, with per-device tokens so revocation is meaningful.

## 5. Module deep-dives

### Fulfillment

The first thing being built. The fulfillment system is responsible for turning user intent ("this media item should exist in the library") into concrete files on disk. Ingestion is one stage of that pipeline; fulfillment is the broader orchestration layer.

#### Request and fulfillment pipeline

```
Request  →  Resolution  →  Acquisition (optional)  →  Ingest  →  Transcode  →  Library
```

**Responsibilities:**

- **Request tracking.** Users request media from clients (tvOS, iOS, etc.). Requests are durable, observable, and part of the core data model.
- **Resolution.** A request is mapped to a canonical media identity (TMDB/TVDB entry). This step is explicit and versioned — a re-resolution is a deliberate, observable action, not a silent overwrite.
- **Fulfillment planning.** Kino determines how a request could be satisfied based on configured providers and current library state. A request may already be satisfied; it may map to a file the user can supply directly; it may require an external provider to do work.
- **Acquisition (optional, pluggable).** If the media is not already available, Kino delegates to user-configured providers to obtain it. Kino itself ships only with local and manual paths.
- **Ingestion.** Once a file exists, it flows through the standard ingestion pipeline: identification, metadata enrichment, placement in the canonical library layout, hand-off to transcode.

#### Provider interface

Kino defines a narrow, stable interface for fulfillment providers. The contract, simplified: *given a media request, can you produce a file or candidate files that satisfy it?*

First-party providers cover the cases Kino is opinionated about:

- **Disc rip import.** Take MakeMKV output (or similar) and ingest it.
- **Watch folders.** A directory the user drops files into. Kino picks them up, identifies them, and processes them.
- **Manual import.** A user-driven flow in the admin UI for the awkward cases the automated paths don't handle.

Beyond those, the provider interface is the product. Users who run their own external tooling implement the interface and Kino orchestrates around it. Kino does not bundle, configure, or recommend specific external providers.

This separation is the point: Kino owns the lifecycle of media within the system, but not the sourcing of that media.

### Transcoding

Replaces general-purpose tools like Tdarr. The goal is to produce a small set of efficient outputs per title, optimized for direct play on the target clients.

**Approach:**

- **Quality-targeted, not bitrate-targeted.** Encode to a VMAF target (e.g. 95) rather than a fixed bitrate. Per-title bitrate, derived from content complexity.
- **Hardware-accelerated by default.** VAAPI, NVENC, VideoToolbox, QSV — detected and used where available. Software fallback (libx265, libsvtav1) when not.
- **Codec policy:** AV1 where the hardware can decode it on the target client, HEVC otherwise, H.264 as the universal fallback. HDR preserved where supported, tone-mapped to SDR otherwise.
- **Output set per title:** typically one "original" (passthrough or remux), one "high" (target client native), one "compatibility" (H.264/AAC). Chosen based on what the user's clients actually need, not a theoretical matrix.
- **Idempotent and resumable.** A transcode job is a deterministic function of inputs and settings. Re-running it is safe; interrupting it doesn't corrupt anything.

### Playback server

Replaces Jellyfin's server. Smaller surface area: serve metadata, serve streams, track playback state, manage sessions.

- Direct play is the default and the goal. Transcode-on-the-fly exists for unanticipated client/network combinations but is not the steady state — the transcode pipeline should have already produced what the client wants.
- Playback state (resume positions, watched flags) is per-user, syncs across devices, and is owned by the server.
- Subtitle handling is first-class: extracted, indexed, and served separately from the video stream.

### Native Apple clients

Built alongside the server. Swift, SwiftUI where it earns its keep, AVFoundation for playback. The clients are designed for a server they trust — which means less defensive code, more direct integration, and fewer "compatibility shim" layers than a generic Jellyfin client needs.

Shared client logic (API bindings, models, playback coordination) lives in a Swift package consumed by all three apps.

## 6. Technical constraints and targets

These are the conditions Kino is being designed to handle well from day one. They shape architecture decisions throughout.

- **Library scale:** tens of thousands of episodes and thousands of films, multi-TB on disk. The catalog must remain responsive at that size.
- **Resolution and HDR:** 4K HDR10 and Dolby Vision (Profile 5 and 8) are first-class. The transcode pipeline preserves HDR metadata; the server signals it correctly to clients; the clients render it natively.
- **Concurrency:** comfortably support a handful of concurrent streams from a single server, including at least one 4K direct play, with headroom for a background transcode.
- **Hardware:** target a modern consumer-grade homelab — a single x86 box with an iGPU or discrete GPU capable of hardware decode/encode. Not a NAS appliance; not a datacenter.
- **Network:** LAN streaming is the primary case. Remote access is supported but not the design center; it works through the same API with bandwidth-aware variant selection.
- **Storage layout:** Kino owns its library directory structure. It does not try to coexist with a hand-curated layout. Import is a one-way migration, not an ongoing reconciliation.

## 7. Roadmap

Rough sequencing, not a schedule. Each phase is "good enough to use daily for that slice."

**Phase 1 — Fulfillment and library.** Request tracking and resolution. Provider interface with first-party providers (disc rip, watch folder, manual import). Library catalog with metadata enrichment. At the end of this phase, Kino can take a request from a user and turn it into an organized, queryable library entry — given a configured way to source the file.

**Phase 2 — Playback server.** Catalog API, HLS streaming with direct play, playback state. A minimal admin web UI. At the end of this phase, Kino can be pointed at a library and serve it to a generic HLS client.

**Phase 3 — Transcoding pipeline.** VMAF-targeted per-title encoding, hardware acceleration, the output-set policy. Integrated with the ingestion path so new files land transcoded.

**Phase 4 — Native Apple clients.** iOS first (highest leverage for testing), then tvOS (the actual long-term target), then macOS. Some early client work can happen in parallel with Phase 2 to keep the API honest.

**Phase 5 — Polish.** Multi-user, sharing, remote access hardening, observability. The features that turn it from "works for me" into "works for the household."

## 8. Open questions

The doc deliberately doesn't resolve these. They're the things to think through before or during the relevant phase:

- **Database choice past v1.** SQLite is right for v1; whether and when to support Postgres for larger deployments is an open question.
- **Subtitle policy.** OCR for image-based subs, forced-subtitle detection — how much of this is in scope.
- **Metadata provider strategy.** TMDB is the obvious primary; how to handle gaps (anime, non-English content, home video) without becoming a metadata provider aggregator like Jellyfin did.
- **Update and migration strategy.** Single-binary upgrades are easy; database migrations across breaking schema changes need a real plan before the first user-facing release.
- **What "v1" means publicly.** This doc treats Kino as a personal project. If at some point it's worth releasing publicly, the bar for that — docs, support, breaking-change policy — is a separate decision.

## 9. Principles

When in doubt, decisions get made by leaning on these:

1. **Owned end-to-end beats integrated.** The whole reason Kino exists is to escape the integration tax. New features should reinforce the unified model, not fragment it.
2. **Orchestration, not acquisition.** Kino owns the lifecycle of media within the system, but not the sourcing of that media. External inputs are explicit, user-configured, and replaceable.
3. **Opinionated beats configurable.** Every config option is a decision deferred to the user. Defer only what genuinely varies.
4. **Native beats portable.** Native clients on the platforms Kino targets, native codecs on the hardware Kino runs on. Lowest-common-denominator is a non-goal.
5. **Fewer features, done well.** The patchwork stack has every feature. Kino's job is to do the ones that matter, better.
6. **Rust where it pays off.** Performance-sensitive paths, correctness-sensitive paths, and the data model. Not as a hair-shirt across the whole project.
