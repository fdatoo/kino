# Phase 3 — Transcoding Pipeline

Status: Draft
Date: 2026-05-11
Phase: 3 (per `docs/kino-vision.md` §7)
Linear milestone: Phase 3

## Context

Phase 2 shipped the playback server: catalog API, HLS master/media playlists,
byte-range delivery over the raw source file, playback state, sessions. Today
the master playlist exposes a single variant that streams the source bytes
in-place. `transcode_outputs` exists as a schema but is unused by playback;
`kino-transcode` is a no-op handoff stub.

Phase 3 replaces the no-op with a real, durable transcoding pipeline. After
this phase an ingested file lands transcoded automatically: a planner decides
the output set, an FFmpeg-backed encoder produces each variant as fMP4/CMAF on
disk, and the server emits a real multi-variant master playlist. A live
transcoding path handles client/network combinations the pre-encoded set
doesn't cover, backed by a cache with LRU + TTL eviction. HDR and Dolby Vision
are first-class: preserved on copy, downgraded honestly when re-encode is
required.

The pipeline is the canonical realization of the vision's transcoding goals
(§5 of the vision doc): quality-targeted (VMAF), hardware-accelerated by
default, idempotent and resumable, with an opinionated three-variant output
set per title.

## Scope

In scope:

- Multi-backend encoder abstraction with software fallback (always) and
  hardware backends for Linux Intel iGPU (QSV with VAAPI fallback) and macOS
  (VideoToolbox). Architecture supports future NVENC.
- Durable transcode job model (state machine, queue, scheduler) backed by
  SQLite, with resource-typed lanes so a CPU job and a GPU job can run
  concurrently but two jobs on the same backend cannot.
- `OutputPolicy` trait with a configurable default that yields three variants:
  Original (passthrough remux), High (HEVC 10-bit, AAC), Compatibility (H.264,
  AAC, ≤1080p).
- Sample-based VMAF curve fitting (ab-av1 style) for CRF selection. Per-codec
  CRF clamps.
- fMP4/CMAF on-disk segmentation produced at encode time. One directory per
  transcode output; static segment files served directly.
- HDR and Dolby Vision handling: preserve on stream-copy paths; encode to
  HDR10 (losing DV enhancement layer) when re-encode is needed; tone-map to
  SDR for Compatibility. Durable record of color downgrades.
- On-the-fly live transcoding for unanticipated profiles, session-scoped with
  a content-addressed cache (separate `ephemeral_transcodes` table, LRU + TTL
  + size cap), configurable off-switch, active-encode dedup.
- Server master/media playlist construction over real `transcode_outputs`,
  with a fallback to Phase 2 byte-range-over-source when no outputs exist yet
  for a freshly ingested file.
- Admin and CLI operational surface: job listing, cancel, replan, retranscode,
  cache stats and purge, encoder backend introspection.
- A typed FFmpeg invocation wrapper (`FfmpegEncodeCommand`, `FfmpegVmafCommand`)
  so backend code is self-documenting and snapshot-testable.

Out of scope (deferred):

- Apple-client-specific behavior (Phase 4).
- NVENC backend implementation (architecture supports it; ship later).
- Per-user transcode preferences or per-device profiles.
- Distributed encode workers.
- Telemetry export (Prometheus/OTLP) — vision §10 polish.
- Subtitle burn-in.
- DV → DV re-encode (not possible with current open-source tooling).
- Reactive replan when a source file on disk is replaced.

## Decisions

1. **Scope:** full vision for Phase 3 — steady-state encode pipeline plus
   on-the-fly fallback plus first-class HDR/DV handling.
2. **Hardware target:** multi-backend abstraction from day one. Backends
   shipped: software (CPU), QSV/VAAPI (Linux Intel iGPU), VideoToolbox (macOS).
3. **Output policy:** configurable trait with a sane default (three variants:
   passthrough Original, HEVC 10-bit High, H.264 ≤1080p Compatibility).
4. **VMAF strategy:** sample-based curve fit. Encode N short samples at trial
   CRFs, fit a curve, pick the CRF that hits the target VMAF, encode once.
5. **HLS packaging:** fMP4/CMAF segments on disk produced at encode time.
   Apple-aligned, cleanest cache behavior, simplest server.
6. **Concurrency:** resource-typed lanes. Each backend declares its lane
   (`cpu`, `gpu_intel`, `gpu_videotoolbox`, future `gpu_nvidia`); one job per
   lane runs concurrently.
7. **On-the-fly cache:** separate `ephemeral_transcodes` table keyed by a
   canonical `TranscodeProfile` hash, LRU + size cap + TTL eviction,
   in-memory active-encode registry for dedup, configurable off-switch.
8. **HDR/DV:** preserve metadata on stream copy; encode to HDR10 (lose DV
   enhancement) when re-encode is required; tone-map to SDR for Compatibility.
   Persist a `transcode_color_downgrades` row for any downgrade.
9. **FFmpeg invocation:** typed builder structs per operation, rendered to
   both `tokio::process::Command` and a shell-quoted `Display` form for logs
   and snapshot tests.

## Crate ownership and module layout

`kino-transcode` owns the entire pipeline. Other crates use it via a small
public API (`TranscodeService`).

```
kino-transcode/
├── lib.rs                      // crate doc, public re-exports
├── error.rs                    // crate Error enum (see §"Error model")
├── service.rs                  // TranscodeService — public entrypoint
├── plan/
│   ├── policy.rs               // OutputPolicy trait + DefaultPolicy
│   ├── variant.rs              // PlannedVariant, VariantKind
│   └── profile.rs              // canonical TranscodeProfile (cache key shape)
├── encoder/
│   ├── mod.rs                  // Encoder trait, Capabilities, LaneId
│   ├── software.rs             // libx264 / libx265 / libsvtav1
│   ├── qsv.rs                  // Intel QSV
│   ├── vaapi.rs                // Linux VA-API fallback
│   ├── videotoolbox.rs         // macOS
│   ├── registry.rs             // EncoderRegistry, lookup by lane + plan fit
│   └── detect.rs               // runtime detection at startup
├── pipeline/
│   ├── ffmpeg.rs               // FfmpegEncodeCommand, FfmpegVmafCommand
│   ├── runner.rs               // spawn, progress, cancellation, integrity
│   ├── segment.rs              // HlsOutputSpec, segment integrity checks
│   └── vmaf.rs                 // sample selection + curve fit
├── job/
│   ├── state.rs                // JobState enum, transition rules
│   ├── store.rs                // SQLx queries against transcode_jobs
│   └── scheduler.rs            // resource-lane dispatch loop, recovery
├── ephemeral/
│   ├── cache.rs                // ephemeral_transcodes table access
│   ├── eviction.rs             // LRU + size + TTL sweeper
│   └── live.rs                 // session-scoped pipe-to-HLS encoder
└── tests/                      // integration tests (synthetic small clips)
```

Public surface:

- `TranscodeService::new(db, config) -> Self` — constructed at server boot;
  spawns the scheduler task. Holds the `EncoderRegistry` and the
  `ActiveEncodes` registry.
- `TranscodeService::submit(source_file_id) -> Result<Vec<JobId>>` —
  plan + enqueue the policy output set. Idempotent (UNIQUE on
  `(source_file_id, profile_hash)`).
- `TranscodeService::cancel(job_id)`, `::replan(source_file_id)`,
  `::retranscode(source_file_id)` — operator actions.
- `TranscodeService::live_segment(source_file_id, profile, segment_n)
  -> Result<SegmentResponse>` — on-the-fly segment fetch. May start a new
  live encode or join an active one.
- `TranscodeService::watch_completion(source_file_id)
  -> impl Stream<Item = JobEvent>` — admin UI and tests.

`kino-server` calls `submit` from the ingestion handoff (replacing today's
`NoopTranscodeHandOff`) and `live_segment` from streaming endpoints.
`kino-admin` uses the operator actions and the `watch_completion` stream.
No other crate touches internals.

## Encoder backend abstraction

```rust
pub enum EncoderKind { Software, Qsv, Vaapi, VideoToolbox /*, Nvenc */ }

pub enum LaneId { Cpu, GpuIntel, GpuVideoToolbox /*, GpuNvidia */ }

pub trait Encoder: Send + Sync {
    fn kind(&self) -> EncoderKind;
    fn lane(&self) -> LaneId;
    fn capabilities(&self) -> &Capabilities;
    fn supports(&self, plan: &PlannedVariant) -> bool;
    fn build_command(&self, ctx: &EncodeContext) -> FfmpegEncodeCommand;
}
```

`Capabilities` is declared per backend (codec set, max width/height, 10-bit
support, HDR support, DV passthrough). `detect::available_encoders()` runs
at startup, probes `ffmpeg -hide_banner -hwaccels` and trial-encodes a tiny
synthetic input per candidate backend, and returns the live
`Vec<Box<dyn Encoder>>`. Each detected backend logs an `info` line.

`EncoderRegistry` indexes encoders by `LaneId` and exposes
`select(&PlannedVariant) -> Option<&dyn Encoder>` using a static preference
order: prefer hardware that supports the plan; fall back to software.

## FFmpeg invocation: typed builders

`pipeline/ffmpeg.rs` provides domain-narrowed typed wrappers. Each is built
from typed enums (`VideoCodec`, `ColorOutput`, `Preset`, `VideoFilter`, …),
exposes `into_command() -> tokio::process::Command` and `to_args() ->
Vec<OsString>`, and implements `Display` rendering the canonical shell-quoted
argv on a single line.

`FfmpegEncodeCommand` is produced by `Encoder::build_command(&EncodeContext)`
and consumed by `pipeline::runner`. `FfmpegVmafCommand` is produced by
`pipeline::vmaf` for sample measurement. The runner never builds argv
itself; encoders never spawn processes themselves.

Each enum variant carries a `///` doc explaining the underlying flag (e.g.
`VideoCodec::Hevc` documents `-c:v libx265` / equivalent hardware encoder
and 10-bit/HDR semantics). Encoder backend call sites read like
domain-level code:

```rust
FfmpegEncodeCommand::new(self.binary.clone(), InputSpec::file(ctx.source_path()))
    .video(VideoOutputSpec {
        codec: VideoCodec::Hevc,
        crf: Some(ctx.chosen_crf()),
        preset: Preset::Medium,
        bit_depth: 10,
        color: ColorOutput::from_plan(&ctx.plan().color),
        max_resolution: ctx.plan().max_resolution(),
    })
    .audio(AudioOutputSpec::stereo_aac_with_surround_passthrough())
    .add_filter_if(ctx.needs_tonemap(), VideoFilter::HdrToSdrTonemap)
    .add_filter_if(ctx.needs_scale(),   VideoFilter::Scale(ctx.plan().max_resolution()))
    .hls(HlsOutputSpec::cmaf_vod(ctx.output_dir(), Duration::from_secs(6)))
```

Snapshot tests assert the `Display` form per `(backend × scenario)`. Any
encoder flag change is a deliberate snapshot review.

## Output policy and planner

```rust
pub trait OutputPolicy: Send + Sync {
    fn plan(&self, source: &SourceContext) -> Vec<PlannedVariant>;
}

pub struct PlannedVariant {
    pub kind: VariantKind,             // Original | High | Compatibility
    pub codec: VideoCodec,
    pub container: Container,          // Fmp4Cmaf
    pub width: Option<u32>,            // None = source resolution
    pub bit_depth: u8,
    pub color: ColorTarget,            // Hdr10 | Sdr (DV preserve via separate flag)
    pub audio: AudioPolicy,
    pub vmaf_target: Option<f32>,      // None for passthrough
}
```

`DefaultPolicy` yields:

- **Original** — `-c copy` passthrough remuxed into fMP4. No re-encode. Audio
  copied. HDR and DV metadata preserved when the source carries them (HEVC).
- **High** — HEVC 10-bit, AAC stereo + surround passthrough when present,
  source resolution, HDR10 when source was HDR or DV (DV enhancement layer
  lost), SDR otherwise. VMAF target 95.
- **Compatibility** — H.264 8-bit, AAC stereo, downscaled to 1080p if source
  exceeds it, SDR (tone-mapped from HDR/DV when needed). VMAF target 90.

The planner produces a `PlannedVariant` per kind, hashes the canonical form
to compute `profile_hash`, and writes a `transcode_jobs` row per variant
(deduped by `UNIQUE(source_file_id, profile_hash)` so re-ingest is idempotent).

Policy is loaded from `kino.toml` under `[transcode.policy]`.

## Job state machine

New migration `0025_transcode_jobs.sql`:

```sql
CREATE TABLE transcode_jobs (
    id              BLOB PRIMARY KEY,
    source_file_id  BLOB NOT NULL REFERENCES source_files(id) ON DELETE CASCADE,
    profile_json    TEXT NOT NULL,
    profile_hash    BLOB NOT NULL,
    state           TEXT NOT NULL,
    lane            TEXT NOT NULL,
    attempt         INTEGER NOT NULL DEFAULT 0,
    progress_pct    INTEGER,
    last_error      TEXT,
    next_attempt_at INTEGER,
    created_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL,
    started_at      INTEGER,
    completed_at    INTEGER,
    UNIQUE(source_file_id, profile_hash)
);
CREATE INDEX transcode_jobs_state_lane ON transcode_jobs(state, lane);
CREATE INDEX transcode_jobs_dispatch
    ON transcode_jobs(state, lane, next_attempt_at, created_at);
```

States: `Planned → Running → Verifying → Completed | Failed | Cancelled`.
Transitions are atomic SQL updates with `updated_at` bumps.

Scheduler tick:

1. For each `LaneId` not currently busy in the in-process registry,
   `SELECT … WHERE state='Planned' AND lane=? AND
   (next_attempt_at IS NULL OR next_attempt_at <= now)
   ORDER BY created_at LIMIT 1`.
2. `UPDATE … SET state='Running', started_at=now, attempt=attempt+1`.
3. Spawn a tokio task running the pipeline; on completion or failure,
   transition the row.
4. Failed jobs with `attempt < max_attempts` and a `Transient` error class
   are re-set to `Planned` with `next_attempt_at = now + backoff`.

Cancellation: set `Cancelled` on the row, signal the in-process task's kill
oneshot. The runner sends FFmpeg `SIGTERM` then `SIGKILL` after grace.

Crash recovery: on startup, reset rows in `Running` (whose process is no
longer alive) to `Planned`, incrementing `attempt`.

## FFmpeg pipeline runner

`pipeline::runner` owns:

- **Spawn** — `FfmpegEncodeCommand::into_command()` plus stdio wiring
  (`-progress pipe:1`, stderr captured to a ring buffer).
- **Progress** — parse `key=value\n` lines from stdout into
  `Progress { frame, fps, time_us, speed }`; update `transcode_jobs.progress_pct`
  at most 1Hz.
- **Cancellation** — `tokio::select!` over a oneshot kill channel and
  `child.wait()`. On kill: SIGTERM → 2s grace → SIGKILL.
- **Output integrity** — on exit 0, assert each declared segment exists and
  is non-zero; assert the playlist file is valid.
- **Error classification** — exit codes map to `Transient`
  (resource-exhausted, GPU-busy) or `Permanent` (codec error, invalid input)
  for retry policy.

Pipeline execution order for a single job:

```
probe (kino-fulfillment::FfprobeFileProbe; reused, not duplicated)
  -> plan already resolved at submit-time, no re-plan during run
  -> if vmaf_target: sample-based VMAF curve fit -> chosen CRF
  -> main encode (FfmpegEncodeCommand)
  -> integrity check
  -> persist transcode_outputs row
  -> Verifying -> Completed
```

## VMAF sampling

For each non-passthrough variant:

1. **Sample selection** — pick `sample_count` samples of `sample_seconds`
   each at evenly distributed offsets (25/50/75% by default for N=3).
2. **Trial encodes** — encode each sample at the configured `trial_crfs`
   (default `[18, 24, 30]`) using the same encoder backend the main encode
   will use.
3. **VMAF measurement** — `FfmpegVmafCommand` runs `libvmaf` per trial,
   yielding mean VMAF.
4. **Curve fit** — linear `vmaf = a*crf + b` per sample; average across
   samples; solve for `crf` at the target VMAF.
5. **Clamp** — bound to `crf_clamp_<codec>` so a pathological source can't
   yield an absurd CRF.
6. **Main encode** at the chosen CRF.

Sample VMAFs, chosen CRF, and a final-clip spot-check VMAF land in
`transcode_outputs.encode_metadata_json`.

Sample encoding runs in the same lane as the main encode (a sample on QSV
occupies the QSV lane). Sampling overhead budget: ~10% of main encode time.

## fMP4 / CMAF packaging

Storage layout under the library directory, mirroring source layout:

```
<library>/Movies/Some Movie (2019)/
├── source.mkv
├── transcodes/
│   ├── <transcode_output_id>-original/
│   │   ├── init.mp4
│   │   ├── seg-00001.m4s … seg-NNNNN.m4s
│   │   └── media.m3u8
│   ├── <transcode_output_id>-high/
│   │   └── …
│   └── <transcode_output_id>-compat/
│       └── …
```

Canonical FFmpeg HLS args (HEVC example):

```
-c:v libx265 -crf 23 ...
-c:a aac -b:a 192k
-f hls -hls_segment_type fmp4 -hls_time 6
-hls_playlist_type vod
-hls_segment_filename 'seg-%05d.m4s'
-hls_fmp4_init_filename 'init.mp4'
media.m3u8
```

`transcode_outputs` schema additions (migration `0026_transcode_outputs_packaging.sql`):

- `directory_path` — relative-to-library directory containing init,
  segments, and playlist.
- `playlist_filename` — usually `media.m3u8`.
- `init_filename` — usually `init.mp4`.
- `encode_metadata_json` — CRF, VMAF measurements, encoder kind, ffmpeg
  version, duration, color metadata.

## HDR and Dolby Vision

**Passthrough.** `Original` uses `-map 0 -c copy`. DV RPU, HDR10 master
display and max CLL all preserved (assuming HEVC source). If the source is
AV1 or H.264, Original still passes through them; only HEVC can carry DV.

**HDR10 preserve on re-encode.** When the High variant requires re-encoding
a HDR/DV source, ffprobe extracts master display + max CLL at planning
time. The encoder backend emits `-color_primaries bt2020 -color_trc smpte2084
-colorspace bt2020nc` plus codec-specific master-display params (e.g.
`-x265-params master-display=…:max-cll=…`). DV enhancement layer is not
re-encoded.

**Tone-map to SDR.** For Compatibility (and any other `ColorTarget = Sdr`
from HDR source), the runner prepends:

```
-vf zscale=t=linear:npl=100,format=gbrpf32le,
    zscale=p=bt709,
    tonemap=tonemap=hable:desat=0,
    zscale=t=bt709:m=bt709:r=tv,
    format=yuv420p
```

QSV / VideoToolbox encoders that require hardware-mapped input chain through
`hwdownload,format=…` after decode, or use the encoder's native tone-map
filter — chosen by the backend's `build_command`.

**Downgrade record.** New table `transcode_color_downgrades` (migration
`0027_transcode_color_downgrades.sql`):

```sql
CREATE TABLE transcode_color_downgrades (
    transcode_output_id BLOB PRIMARY KEY REFERENCES transcode_outputs(id) ON DELETE CASCADE,
    kind                TEXT NOT NULL,   -- 'dv_to_hdr10' | 'hdr10_to_sdr' | 'dv_to_sdr'
    note                TEXT,
    created_at          INTEGER NOT NULL
);
```

Surfaced via admin endpoint; durable provenance.

## On-the-fly cache and live sessions

Migration `0028_ephemeral_transcodes.sql`:

```sql
CREATE TABLE ephemeral_transcodes (
    id              BLOB PRIMARY KEY,
    source_file_id  BLOB NOT NULL REFERENCES source_files(id) ON DELETE CASCADE,
    profile_hash    BLOB NOT NULL,
    profile_json    TEXT NOT NULL,
    directory_path  TEXT NOT NULL,
    size_bytes      INTEGER NOT NULL,
    created_at      INTEGER NOT NULL,
    last_access_at  INTEGER NOT NULL,
    UNIQUE(source_file_id, profile_hash)
);
CREATE INDEX ephemeral_transcodes_lru ON ephemeral_transcodes(last_access_at);
```

`TranscodeProfile` is the canonical cache key — same shape as planner
`PlannedVariant`, content-hashed. Profiles are normalized to a small set so
near-identical client requests hit the same row.

Live segment request flow
(`GET /api/v1/stream/live/{source_file_id}/{profile}/{seg}.m4s`):

1. Look up `(source_file_id, profile_hash)` in `transcode_outputs` → if hit,
   redirect to (or serve directly from) the durable variant.
2. Else look up in `ephemeral_transcodes` → if hit, bump `last_access_at`,
   serve segment file.
3. Else look up in the in-memory `ActiveEncodes` registry → if a live encode
   for the same key is in flight, await its watch channel for segment N,
   then serve.
4. Else start a new live encode: register in `ActiveEncodes` with refcount,
   spawn an `FfmpegEncodeCommand` writing into `<cache_root>/<new_id>/`,
   wait for segment N, serve. On encode finish (or cancellation), move row
   from `ActiveEncodes` into `ephemeral_transcodes`.

Live encodes use the scheduler's resource lanes with priority `Live` (preempt
new `Planned` jobs at dispatch, do not kill running ones). When at least
one playback session is active, the scheduler reserves one lane
(`transcode.scheduler.reserve_live_lane`, default `cpu`) for live work so a
long batch ingest can't starve playback.

Eviction sweeper runs every `eviction_tick_seconds`:

- TTL cull: rows with `last_access_at < now - max_age_seconds`.
- Size cull: while `sum(size_bytes) > max_size_bytes`, delete LRU rows.
- Triggered immediately after a write that pushes us over the size cap.

Off-switch: `[transcode.ephemeral] enabled = false` short-circuits step 4
(no new live encodes; existing cached and durable hits still serve).

## Server integration: master playlist construction

Phase 2's master playlist builds one variant from source-file probe data.
Phase 3 enumerates `transcode_outputs` for the source file and emits one
`#EXT-X-STREAM-INF` per output, populating `BANDWIDTH`, `CODECS`,
`RESOLUTION`, and `VIDEO-RANGE` from `encode_metadata_json`.

Routes:

- `GET /api/v1/stream/items/{id}/master.m3u8` — list all variants for the
  media item's selected source file.
- `GET /api/v1/stream/transcodes/{transcode_output_id}/media.m3u8` —
  per-variant media playlist.
- `GET /api/v1/stream/transcodes/{transcode_output_id}/init.mp4`,
  `…/seg-{n:05}.m4s` — init segment and segments served directly from disk.
- `GET /api/v1/stream/live/{source_file_id}/{profile}/…` — on-the-fly
  variant served per the live flow above.
- Subtitle routes unchanged from Phase 2.

When no `transcode_outputs` exist for a source file (fresh ingest, transcode
in flight), the master playlist falls back to the Phase 2 byte-range
behavior as a single Original-equivalent entry. Preserves the "watch
immediately after ingest" UX during the transcode wait.

Variant selection is client-side; the server emits all variants. No
User-Agent sniffing. Live profile URLs are the explicit per-client opt-in.

**API change.** Phase 2's `{variant_id}` path segment in
`/api/v1/stream/items/{id}/{variant_id}/master.m3u8` is removed; the master
manifest itself enumerates variants. Per ADR-0004 this is a v1 change
requiring either an `Accept-Version` bump or a deprecation window — to be
decided at implementation time.

## Admin and operational surface

New endpoints, consumed by `kino-admin` and `kino-cli`:

- `GET /api/v1/admin/transcodes/jobs[?state=&lane=&source_file_id=]`
- `GET /api/v1/admin/transcodes/jobs/{id}` — full job detail including
  progress, last error, attempts.
- `POST /api/v1/admin/transcodes/jobs/{id}/cancel`
- `POST /api/v1/admin/transcodes/sources/{source_file_id}/retranscode` —
  delete outputs + replan + re-enqueue.
- `POST /api/v1/admin/transcodes/sources/{source_file_id}/replan` — replan
  only (after a policy change).
- `GET /api/v1/admin/transcodes/encoders` — list detected backends and
  capabilities (debugging hardware detection).
- `GET /api/v1/admin/transcodes/cache` — ephemeral cache stats (size,
  oldest, hit-rate counters).
- `POST /api/v1/admin/transcodes/cache/purge`

`kino-cli` additions:

- `kino transcode jobs [list | show <id>]`
- `kino transcode cancel <job_id>`
- `kino transcode retry <job_id>`
- `kino transcode retranscode <source_file_id>`
- `kino transcode encoders`

Admin UI work itself is Phase 5 polish; Phase 3 ships the endpoints so curl
and the CLI can drive the system.

Tracing: spans per job with fields `job_id`, `source_file_id`, `variant_kind`,
`encoder_kind`, `lane`. State transitions log `info`. Progress logs `debug`
(1Hz). Failures log `error` with structured error.

## Configuration

`kino.toml` additions, all with defaults:

```toml
[transcode]
ffmpeg_path        = "ffmpeg"
ffprobe_path       = "ffprobe"
work_dir           = "<library>/.kino/transcode-work"
max_attempts       = 3
backoff_seconds    = 60

[transcode.policy]
high.codec         = "hevc"
high.bit_depth     = 10
high.vmaf_target   = 95
high.preset        = "medium"
compat.codec       = "h264"
compat.vmaf_target = 90
compat.max_height  = 1080

[transcode.encoders]
allow              = ["software", "qsv", "vaapi", "videotoolbox"]
detect_at_startup  = true

[transcode.vmaf]
sample_count       = 3
sample_seconds     = 12
trial_crfs         = [18, 24, 30]
crf_clamp_hevc     = [16, 32]
crf_clamp_h264     = [18, 30]

[transcode.scheduler]
reserve_live_lane  = "cpu"
recovery_on_boot   = true

[transcode.ephemeral]
enabled               = true
cache_root            = "<library>/.kino/ephemeral"
max_size_bytes        = 25_000_000_000
max_age_seconds       = 21_600
eviction_tick_seconds = 60
```

Env var overrides follow `KINO_TRANSCODE__…`. CLAUDE.md's documented env
var list is extended accordingly.

## Error model

```rust
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("ffmpeg binary not found at {0}")]
    BinaryMissing(PathBuf),
    #[error("encoder backend {backend:?} unavailable: {reason}")]
    BackendUnavailable { backend: EncoderKind, reason: String },
    #[error("source file {0} not found in database")]
    SourceFileMissing(Id),
    #[error("ffmpeg exited with status {status}: {stderr_tail}")]
    FfmpegFailed { status: i32, stderr_tail: String },
    #[error("ffmpeg killed by signal during cancellation")]
    Cancelled,
    #[error("vmaf measurement failed: {0}")]
    VmafFailed(String),
    #[error("encoded output failed integrity check: {0}")]
    IntegrityFailed(String),
    #[error("job state transition rejected: {from:?} -> {to:?}")]
    InvalidTransition { from: JobState, to: JobState },
    #[error("ephemeral cache exhausted: {0}")]
    CacheExhausted(String),
    #[error(transparent)]
    Db(#[from] kino_db::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
```

Errors classify into `Transient` (retryable) and `Permanent` via
`Error::is_transient()`. Scheduler uses this for retry policy.

No `anyhow`. Library crate, per CLAUDE.md.

## Testing strategy

- **Unit:** snapshot tests of `FfmpegEncodeCommand::Display` and
  `FfmpegVmafCommand::Display` per encoder × scenario. State machine
  transitions table-tested.
- **Integration (CI):** synthetic ~5s 1080p test clip (generated at test
  setup via `ffmpeg -f lavfi`) drives a real software end-to-end encode +
  VMAF sample + segmentation + integrity check. Fast enough for CI.
- **Integration (hardware):** QSV / VAAPI / VideoToolbox paths gated behind
  `cfg(feature = "hwaccel-tests")` and detected-backend checks. Run on dev
  machines; skipped in CI.
- **DB tests:** `transcode_jobs` and `ephemeral_transcodes` queries use
  `kino-db::test_db()`.
- **Concurrency tests:** scheduler dispatch under multiple lanes via
  in-memory fake encoders that take synthetic wallclock time. Validates
  lane fairness, cancellation propagation, recovery on boot.
- **Ephemeral cache:** sweeper correctness with synthetic rows; LRU
  ordering; size-cap eviction triggered by writes.
- **Phase 3 acceptance:** ingest a multi-track HDR10 source via watch
  folder, observe Original / High / Compatibility outputs land, master
  playlist exposes all three with correct codec/range tags, ffplay direct-
  plays each.

## Phase 3 exit criteria

1. A newly ingested source file is automatically transcoded to the policy
   output set (Original + High + Compatibility by default).
2. The server's master playlist enumerates the produced variants with
   correct `BANDWIDTH`, `CODECS`, `RESOLUTION`, and `VIDEO-RANGE`.
3. A generic HLS client (ffplay, Safari) direct-plays each variant.
4. Re-ingesting the same source is idempotent (no duplicate jobs, no
   re-encoded outputs).
5. Cancelling an in-flight job stops the FFmpeg process and marks the job
   `Cancelled`. The next planned job in the same lane starts within one
   scheduler tick.
6. A live transcoding request for a profile not in the durable set produces
   playable segments and caches them in `ephemeral_transcodes` until
   evicted.
7. Detected encoder backends are visible via
   `GET /api/v1/admin/transcodes/encoders`; on a QSV-capable Linux box at
   least QSV and software backends are listed.
8. HDR10 source produces an HDR10 High variant (color metadata preserved)
   and an SDR Compatibility variant; a `transcode_color_downgrades` row
   exists for the latter.
9. `just build`, `just test`, `just fmt-check`, `just lint` all pass.

## Implementation breakdown (Linear epics)

Each epic is sized to a small handful of issues. Order is roughly
dependency order; some pairs can run in parallel.

1. **Core: job model + scheduler** — `transcode_jobs` migration, `JobState`
   transitions, scheduler dispatch loop with resource lanes, recovery on
   boot. In-memory fake encoder for testing. Replaces `NoopTranscodeHandOff`
   call sites with `TranscodeService::submit`.
2. **Encoder abstraction + software backend** — `Encoder` trait,
   `Capabilities`, `EncoderRegistry`, detection, software backend
   (libx265 / libx264). Snapshot tests of the typed `FfmpegEncodeCommand`.
3. **Pipeline runner** — spawn / progress / cancellation / integrity over
   `FfmpegEncodeCommand`. Error classification.
4. **Output policy + planner** — `OutputPolicy` trait, `DefaultPolicy`,
   `TranscodeProfile`, planner that emits the three-variant set, idempotency
   via `profile_hash`. Config loading.
5. **fMP4 / CMAF packaging** — `HlsOutputSpec`, encode-time segmentation,
   `transcode_outputs` schema additions migration, persistence after a
   successful encode.
6. **VMAF sampling** — sample selection, trial encodes, curve fit, CRF
   clamps, `FfmpegVmafCommand` typed builder.
7. **HDR / DV handling** — color metadata extraction at planning,
   passthrough preservation, HDR10 preserve on re-encode, SDR tone-map
   filter chain, `transcode_color_downgrades` migration and persistence.
8. **QSV / VAAPI backend** — Intel iGPU encoder, hardware detection,
   capability matrix, hwaccel-tests gated integration.
9. **VideoToolbox backend** — macOS encoder, hardware detection, capability
   matrix, hwaccel-tests gated integration.
10. **Server: multi-variant master playlist** — rewrite of
    `stream::master_playlist` over `transcode_outputs`, new per-variant
    media and segment routes, Phase 2 byte-range fallback when no outputs
    exist. API change handling per ADR-0004.
11. **On-the-fly + ephemeral cache** — `ephemeral_transcodes` migration,
    `ActiveEncodes` registry, live segment route, LRU + TTL + size-cap
    sweeper, off-switch, dedup.
12. **Admin + CLI surface** — admin endpoints (jobs, encoders, cache),
    `kino-cli transcode …` subcommands, OpenAPI updates.
13. **Phase 3 acceptance** — end-to-end ingest → transcode → playback
    validation against an HDR10 source on a target machine.

## Open questions (for implementation phase)

- ADR-0004 API change: bump `Accept-Version` or deprecation window for the
  variant-id path-segment removal? Resolve before epic 10.
- Whether `kino-fulfillment::FfprobeFileProbe` is reused as-is by
  `kino-transcode` or lifted into `kino-core`. Decide when epic 4 lands.
- AV1 software encoding (`libsvtav1`) is supported by the abstraction but
  not in the default policy. Worth shipping behind a config toggle in
  Phase 3 or strictly Phase 5? Default: not in policy, but the
  `VideoCodec::Av1` enum variant ships so backends can declare support.
- Per-encoder lane reservation when GPU is at thermal/power limit — leave
  to user via `[transcode.encoders] allow = …` for Phase 3.

## References

- `docs/kino-vision.md` §5 (Transcoding), §7 (Roadmap Phase 3).
- `docs/adrs/0001-sqlite-access-via-sqlx.md` — DB access pattern.
- `docs/adrs/0004-api-versioning-policy.md` — API change handling.
- `docs/agents/specs/2026-05-11-transcode-handoff-stub.md` — current Phase 1
  handoff stub replaced by `TranscodeService::submit`.
- `docs/agents/specs/2026-05-11-media-source-transcode-schemas.md` — Phase 1
  catalog schema this builds on.
