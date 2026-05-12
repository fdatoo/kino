# QSV and VA-API Encoder Backends

## Scope

Implement Intel QSV as the preferred Linux iGPU encoder backend and VA-API as
the fallback backend on the same `GpuIntel` lane. Keep software encoding as the
default non-hardware fallback.

## Steps

1. Extend the typed FFmpeg builder with narrow hardware acceleration, encoder
   override, pixel-format override, and hardware quality flag support.
2. Add QSV and VA-API encoder modules with static capabilities, hardware filter
   translation, and snapshot coverage for SDR, HDR10 preserve, and HDR-to-SDR.
3. Wire runtime detection through `ffmpeg -hwaccels`, render-node checks, and
   tiny trial encodes, registering QSV before VA-API.
4. Add opt-in hardware integration tests behind `hwaccel-tests` and document the
   feature flag.
5. Run build, tests, formatting check, and clippy before committing.
