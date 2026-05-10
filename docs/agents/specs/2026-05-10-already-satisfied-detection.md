# Already Satisfied Detection

Linear: F-241

## Goal

When request resolution selects a canonical identity that is already present in
the library, stop fulfillment immediately. The request should converge on the
existing media item instead of selecting or invoking a provider.

## Scope

- Add the minimal `media_items` table needed to answer "does this canonical
  identity already exist in the library?"
- Keep the full `MediaItem`, `SourceFile`, and `TranscodeOutput` model in
  `F-258`; this issue only creates the early table and canonical identity
  uniqueness constraint.
- Check library presence during identity resolution and provider planning.
- Persist an `already_satisfied` fulfillment plan and transition the request to
  `satisfied`.

## Behavior

The check is by `media_items.canonical_identity_id`. Canonical identity rows on
their own do not imply library presence.

Resolution paths that select an existing library identity write the canonical
identity version, write an `already_satisfied` plan, and transition directly to
`satisfied`. Provider planning performs the same check before provider
validation or ranking so a satisfied request does not fail because of an
irrelevant provider configuration.

## Follow-up

`F-258` should extend the early `media_items` table instead of creating a
conflicting replacement. That issue still owns the core structs, source files,
transcode outputs, and complete catalog constraints.
