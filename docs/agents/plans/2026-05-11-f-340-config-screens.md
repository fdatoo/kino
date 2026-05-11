# F-340 config screens

## Plan

- Add a protected `GET /api/v1/admin/config` endpoint that returns the resolved startup config as read-only DTOs.
- Mask optional secret values and include per-field source labels from current TOML/env/default inspection.
- Generate OpenAPI types, add a `/admin/config` SPA route, and link it from the admin header.
- Cover the endpoint and page with focused tests, then run the required verification gate.
