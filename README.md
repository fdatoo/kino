# Kino

> ⚠️ Personal project, under active development. Not ready for general use.

A purpose-built, end-to-end self-hosted media platform written in Rust.

Kino handles the full media lifecycle — Ingest → Catalog → Transcode → Serve → Play —
as a single binary with one config file and one database. It is designed to replace the
patchwork of separate tools (library manager, transcoder, streaming server) with a
vertically integrated system that owns the problem end-to-end.

Native Apple clients (iOS, macOS, tvOS) are part of the project and live in a separate
repository.

## Goals

- One binary, one config, one database
- Request-driven workflow: express intent from a client device, Kino resolves it into
  the library
- Quality-aware, hardware-accelerated transcoding (VMAF-targeted, not bitrate-targeted)
- Direct play as the default; transcode fallback when needed
- Opinionated scope: personal media libraries, Apple clients, Linux server

## Architecture

Kino is a Rust modular monolith — multiple crates that compile into a single deployable
binary:

| Crate               | Responsibility                                      |
|---------------------|-----------------------------------------------------|
| `kino-core`         | Shared types, errors, configuration, data model     |
| `kino-db`           | Schema, migrations, queries (SQLite)                |
| `kino-fulfillment`  | Request tracking, resolution, provider interface    |
| `kino-library`      | On-disk layout, metadata enrichment (TMDB/TVDB)     |
| `kino-transcode`    | FFmpeg orchestration, hardware acceleration         |
| `kino-server`       | HTTP/gRPC API, streaming, session management        |
| `kino-admin`        | Minimal web UI for configuration and operations     |
| `kino-cli`          | Operational tooling                                 |

## Build

    cargo build --workspace

## Run

    cargo run
