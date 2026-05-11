# Kino Admin Web

This directory contains the React 18 + Vite + TypeScript single-page app for
Kino's admin UI.

## Local development

Install dependencies once:

```sh
pnpm install
```

Start the Vite dev server:

```sh
pnpm dev
```

Run the same web checks used by CI:

```sh
pnpm gen
pnpm typecheck
pnpm lint
pnpm build
```

## API client generation

The typed API client is generated from the committed OpenAPI spec:

```sh
pnpm gen
```

This reads `../../kino-server/openapi.json` and writes
`src/api/schema.ts`. The generated file is ignored by git; update the source
spec with `just openapi` from the repository root before regenerating the web
client. `pnpm build` runs `pnpm gen` first so production builds use the current
committed spec.

## Binary consumption

For this sub-issue the SPA is standalone. A later sub-issue will embed
`dist/` into the `kino-admin` binary, so `pnpm build` is the boundary the Rust
side will consume.

## Device token bootstrap

Until admin authentication is wired in F-304, the local server exposes the token
issuance endpoint without authentication:

```sh
curl -X POST http://127.0.0.1:3000/api/v1/admin/tokens \
  -H 'content-type: application/json' \
  -d '{"label":"admin workstation"}'
```

The response includes the plaintext `token` exactly once. Kino stores only the
SHA-256 hash; later reads from `GET /api/v1/admin/tokens` return token metadata
such as `token_id`, `label`, `last_seen_at`, and `revoked_at`, never the
plaintext token.
