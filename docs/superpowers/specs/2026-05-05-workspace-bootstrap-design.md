# Workspace Bootstrap — Design Spec

**Linear:** F-186  
**Phase:** 0 — Foundations  
**Date:** 2026-05-05

## Done when

- `cargo build --workspace` succeeds on a fresh clone
- `cargo run` exits cleanly after logging "ready"

---

## 1. Directory layout

```
Kino/
├── Cargo.toml           # workspace root
├── Cargo.lock
├── LICENSE
├── README.md
├── docs/
└── crates/
    ├── kino-core/
    ├── kino-db/
    ├── kino-fulfillment/
    ├── kino-library/
    ├── kino-transcode/
    ├── kino-server/
    ├── kino-admin/
    ├── kino-cli/
    └── kino/            # binary entry point
```

All crates live under `crates/`. The repo root holds only workspace-level files and docs.

---

## 2. Workspace Cargo.toml

```toml
[workspace]
members = [
    "crates/kino-core",
    "crates/kino-db",
    "crates/kino-fulfillment",
    "crates/kino-library",
    "crates/kino-transcode",
    "crates/kino-server",
    "crates/kino-admin",
    "crates/kino-cli",
    "crates/kino",
]
resolver = "2"

[workspace.package]
edition = "2024"
version = "0.1.0"
license = "Apache-2.0"

[workspace.dependencies]
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }

[workspace.lints.rust]
unsafe_code = "deny"
unused_imports = "warn"

[workspace.lints.clippy]
all = "warn"
```

---

## 3. Crate skeletons

All 8 library crates follow the same pattern. Their `Cargo.toml` inherits workspace fields and declares no dependencies (except where noted). Their `src/lib.rs` is empty.

**Shared `Cargo.toml` template:**

```toml
[package]
name = "kino-<name>"
version.workspace = true
edition.workspace = true
license.workspace = true

[lints]
workspace = true
```

**Exception — `kino-core`:** adds `serde = { workspace = true }` because it owns the `Config` type.

No library crate depends on another at this stage. That dependency graph is defined as real code lands.

---

## 4. Binary entry point (`crates/kino`)

**`Cargo.toml`:**

```toml
[package]
name = "kino"
version.workspace = true
edition.workspace = true
license.workspace = true

[lints]
workspace = true

[dependencies]
kino-core = { path = "../kino-core" }
kino-db = { path = "../kino-db" }
kino-fulfillment = { path = "../kino-fulfillment" }
kino-library = { path = "../kino-library" }
kino-transcode = { path = "../kino-transcode" }
kino-server = { path = "../kino-server" }
kino-admin = { path = "../kino-admin" }
kino-cli = { path = "../kino-cli" }
tokio = { workspace = true }
tracing = { workspace = true }
tracing-subscriber = { workspace = true }
```

**`src/main.rs`:**

```rust
#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    tracing::info!("ready");
}
```

---

## 5. Config skeleton (`kino-core`)

`kino-core` establishes the `Config` type even though the binary doesn't load it yet. Future work adds fields and loading logic here.

```rust
// crates/kino-core/src/lib.rs
pub mod config {
    #[derive(serde::Deserialize)]
    pub struct Config {}
}
```

---

## 6. License + README

**`LICENSE`:** Apache 2.0, dated 2026, copyright "Fynn Datoo".

**`README.md`:**

```markdown
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
```
