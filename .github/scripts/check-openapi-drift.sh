#!/usr/bin/env sh
set -eu

script_dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
repo="$(CDPATH= cd -- "$script_dir/../.." && pwd)"
committed="$repo/crates/kino-server/openapi.json"
tmp="$(mktemp -d)"
pid=""

cleanup() {
    if [ -n "$pid" ]; then
        kill "$pid" 2>/dev/null || true
    fi
    rm -rf "$tmp"
}

trap cleanup EXIT INT TERM

mkdir -p "$tmp/library"

(
    cd "$repo"
    KINO_DATABASE_PATH="$tmp/kino.db" \
        KINO_LIBRARY_ROOT="$tmp/library" \
        KINO_SERVER__LISTEN="127.0.0.1:18080" \
        KINO_LOG_LEVEL="error" \
        cargo run -p kino
) >"$tmp/kino.log" 2>&1 &
pid="$!"

attempts=0
while [ "$attempts" -lt 1200 ]; do
    if curl -fsS "http://127.0.0.1:18080/api/openapi.json" -o "$tmp/openapi.json" 2>/dev/null; then
        if cmp -s "$committed" "$tmp/openapi.json"; then
            exit 0
        fi

        echo "::error::OpenAPI spec drifted. Run \`just openapi\` and commit crates/kino-server/openapi.json." >&2
        diff -u "$committed" "$tmp/openapi.json" >&2 || true
        exit 1
    fi

    if ! kill -0 "$pid" 2>/dev/null; then
        cat "$tmp/kino.log" >&2
        echo "::error::Kino exited before serving OpenAPI JSON" >&2
        exit 1
    fi

    attempts=$((attempts + 1))
    sleep 0.5
done

cat "$tmp/kino.log" >&2
echo "::error::timed out waiting for Kino to serve OpenAPI JSON" >&2
exit 1
