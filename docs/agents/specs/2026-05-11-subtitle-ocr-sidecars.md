# Subtitle OCR Sidecars

Linear issue: F-334

## Context

Image subtitle tracks are extracted as timestamped image frames before they can
be indexed as text. DECISION F-331 selects Tesseract, invoked as a local
process, as the OCR engine for this slice.

## Scope

`kino-library` owns the library-facing OCR surface:

- `subtitle_image_extraction` describes timestamped image subtitle frames and
  the extractor trait ingestion can implement.
- `subtitle_ocr` shells out to Tesseract with TSV output, aggregates positive
  word confidence values into one cue confidence, and converts extracted frames
  into time-coded OCR cues.
- `SubtitleService` persists OCR-derived subtitle sidecars alongside existing
  text sidecars, with `subtitle_sidecars.provenance` marking whether a row came
  from source text or OCR.

OCR sidecars are JSON so cue timing, text, provenance, and confidence can be
stored without overloading SRT/ASS syntax.
