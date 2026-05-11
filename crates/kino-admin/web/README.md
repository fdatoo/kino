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
pnpm typecheck
pnpm lint
pnpm build
```

## Binary consumption

For this sub-issue the SPA is standalone. A later sub-issue will embed
`dist/` into the `kino-admin` binary, so `pnpm build` is the boundary the Rust
side will consume.
