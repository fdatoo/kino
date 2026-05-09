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
    cargo run
