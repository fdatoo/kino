# Agent artifacts

This directory holds agent-authored planning artifacts so they don't pollute the
product/architecture docs in `docs/`.

| Directory | Purpose                                                                                |
|-----------|----------------------------------------------------------------------------------------|
| `specs/`  | Design specs — one per non-trivial Linear issue or epic. The "what and why."           |
| `plans/`  | Implementation plans — step-by-step execution for a spec. The "how."                   |

## Naming

`YYYY-MM-DD-short-slug.md` (UTC date the file was authored).

## Lifecycle

Specs and plans are working documents — they capture intent at the time they were
written and aren't kept perfectly in sync with the code afterwards. Architectural
decisions that need to outlive the issue belong in `docs/` proper.

## Boundary

- `docs/` — product, vision, architecture (long-lived).
- `docs/agents/` — agent-authored, tied to a Linear issue (working docs).
