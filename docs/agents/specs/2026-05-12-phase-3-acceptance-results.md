# Phase 3 Acceptance Results

Status: Implemented, default-skipped
Date: 2026-05-12

## Coverage

1. Newly ingested source files are transcoded to Original, High, and
   Compatibility outputs.
   - Validated by `phase_3_hdr10_end_to_end_transcodes_policy_outputs`.

2. The master playlist enumerates produced variants with `BANDWIDTH`,
   `CODECS`, `RESOLUTION`, and `VIDEO-RANGE`.
   - Validated by `phase_3_hdr10_end_to_end_transcodes_policy_outputs`.

3. Generic HLS clients can direct-play each variant.
   - Programmatic HLS parsing is covered by
     `phase_3_hls_playlists_parse_for_each_variant`.
   - ffplay and Safari remain manual smoke tests.

4. Re-ingesting the same source is idempotent: no duplicate jobs and no
   duplicate output work.
   - Validated by `phase_3_reingest_idempotency_does_not_duplicate_jobs`.

5. Cancelling an in-flight job stops work, marks it `cancelled`, and allows the
   next planned job in the lane to start within one scheduler tick.
   - Validated by `phase_3_cancellation_and_recovery_reset_running_work`.

6. Live transcoding for a non-durable profile produces playable HLS and persists
   an `ephemeral_transcodes` cache row.
   - Validated by `phase_3_live_transcode_deduplicates_and_caches`.

7. Detected encoder backends are visible through
   `GET /api/v1/admin/transcodes/encoders`.
   - Validated by `phase_3_hdr10_end_to_end_transcodes_policy_outputs`.
   - QSV presence is host-dependent; the test asserts software is present and
     leaves QSV-specific validation to QSV-capable Linux workstations.

8. HDR10 source produces HDR10/PQ durable variants and an SDR Compatibility
   variant with a `transcode_color_downgrades` row.
   - Validated by `phase_3_hdr10_end_to_end_transcodes_policy_outputs`.

9. Workspace verification passes.
   - `just build`: passed on 2026-05-12.
   - `just test`: failed on 2026-05-12 before acceptance tests ran because
     existing `kino-fulfillment::tmdb` unit tests could not bind
     `127.0.0.1:0` in the sandbox (`PermissionDenied`).
   - `just fmt-check`: passed on 2026-05-12.
   - `just lint`: passed on 2026-05-12.

## Caveats

- The acceptance tests are gated behind `acceptance-tests` and ignored by
  default. Run them with:

  ```sh
  cargo test -p kino-server --features acceptance-tests phase_3_acceptance
  ```

- The tests require a real `ffmpeg` with `libx265`, and the `kino` binary must
  exist under `target/debug` from `cargo build -p kino` or `just build`.
- The current product does not run the watch-folder provider as a background
  daemon, and the real binary cannot override TMDB base URLs from `kino.toml`.
  The harness therefore seeds accepted catalog source rows directly, then drives
  production transcode planning, scheduling, admin, live-cache, and stream APIs
  through the real HTTP server.
- ffplay and Safari playback remain manual smoke tests; this harness performs
  inline master/media playlist parsing for each produced variant.
