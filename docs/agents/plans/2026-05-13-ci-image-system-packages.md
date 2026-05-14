# CI Image System Packages

## Goal

Remove the repeated `apt-get update && apt-get install` cost from the main CI job
by running it inside a GHCR image that already contains Kino's native CI tools.

## Plan

1. Add `.github/ci.Dockerfile` based on Ubuntu 24.04 with `ffmpeg`, `just`,
   `tesseract-ocr`, Rust/rustup, and basic build utilities.
2. Add a `CI Image` workflow that validates the Dockerfile on PRs and publishes
   `ghcr.io/fdatoo/kino-ci:latest` plus a SHA tag on pushes to `main`.
3. Update the Rust CI job to use that image as its job container and remove the
   per-run system package install step.

## Verification

- Validate workflow YAML syntax locally.
- Run repository formatting checks after the workflow changes.
- The image must be published once with the new `CI Image` workflow before the
  updated `CI` workflow can pull it on PRs.
