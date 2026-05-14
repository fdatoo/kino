set shell := ["sh", "-cu"]

setup:
    git config core.hooksPath .githooks

fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all -- --check

lint:
    cargo clippy --workspace --all-targets -- -D warnings

test:
    cargo test --workspace

build:
    cargo build --workspace

run:
    cargo run -p kino

openapi:
    tmp="$(mktemp -d)"; \
    mkdir -p "$tmp/library"; \
    KINO_DATABASE_PATH="$tmp/kino.db" \
    KINO_LIBRARY_ROOT="$tmp/library" \
    KINO_SERVER__LISTEN="127.0.0.1:18080" \
    KINO_LOG_LEVEL="error" \
    cargo run -p kino > "$tmp/kino.log" 2>&1 & \
    pid="$!"; \
    trap 'kill "$pid" 2>/dev/null || true; rm -rf "$tmp"' EXIT INT TERM; \
    for _ in $(seq 1 240); do \
        if curl -fsS "http://127.0.0.1:18080/api/openapi.json" -o crates/kino-server/openapi.json.tmp 2>/dev/null; then \
            mv crates/kino-server/openapi.json.tmp crates/kino-server/openapi.json; \
            exit 0; \
        fi; \
        sleep 0.5; \
    done; \
    cat "$tmp/kino.log" >&2; \
    exit 1
