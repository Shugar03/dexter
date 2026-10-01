# SDD — Vision fallback (OCR)

Status: implemented (Item 1 of the maturity plan).

## Problem

Apps with poor or absent accessibility trees — Electron, custom toolkits,
canvas-based UIs — are effectively invisible to Dexter. The spec's §8.3
`VisionProvider` seam existed on paper only.

## Design

### `dexter-vision` crate

- `VisionProvider` trait — synchronous, on-device only:
  `recognize(&self, png: &[u8]) -> Result<Vec<VisionToken>, VisionError>`.
- `VisionToken { text, bounds: NormRect, confidence }` — `NormRect` is
  normalized 0..1 with **bottom-left** origin (Vision native).
- `VisionError::{Unsupported, Failed}` — no provider → `Unsupported`;
  never fabricated elements.
- `platform_provider()` — `Some(AppleVision)` on macOS, `None` elsewhere.
- `token_rect` / `tokens_to_elements` — pure mapping to Dexter screen
  points (top-left origin), handling Retina scale via
  `img_px / window_pt`. Bounds rounded to whole points.
- `png_dimensions` — IHDR parse, no pixel decode.

### Apple Vision provider (`crates/vision/src/apple.rs`)

`VNRecognizeTextRequest` (level `Accurate`, auto language detection) run
through `VNImageRequestHandler::initWithData_options` via `objc2-vision`.
Each observation contributes `topCandidates(1)` → `string`, `confidence`,
`boundingBox`. Fully offline; no model downloads.

### Trigger contract — opt-in only

`ObservationScope.vision: bool` (default `false`). OCR runs only when
requested **and** warranted:

- `ax_limited` — CGWindowList reports windows but AX shows only the app
  shell/menu machinery (Electron, degraded grants), or
- `elements.is_empty()`, or
- `scope.window.is_some()` — canvas regions inside healthy AX apps.

It never fires implicitly on rich app-scoped trees: OCR is a screen
capture plus a recognition pass — expensive and lower-confidence.

### Capture → element mapping

`screenshot::capture_window_region` captures *exactly* the target
window's CGWindowList bounds (deterministic monitor crop — unlike
`capture_app_window`, which lets xcap pick the focused window). The
chosen window is `scope.window` when set, else
`pick_capture_window` (largest layer-0, preferring on-screen — the same
selection the screenshot path always used).

Monitor choice is multi-display aware (`capture_monitor` /
`monitor_pixel_crop` in `dexter-vision`, pure and Linux-testable):
the capture comes from the display whose CGDisplayBounds — the same
global point space as CGWindowList — fully contains the window, and the
pixel crop is offset from that display's origin at its own scale. A
window that straddles displays, sits off every display, or isn't fully
covered by the captured image fails closed (`NotFound`) instead of
returning a clipped crop that would skew the token→point mapping.

Tokens become elements appended after AX elements (ids continue the AX
sequence so `id == index + 1` still holds):

- `source: ElementSource::Ocr`, `role: "text"`, `raw_role: "ocr"`,
  `name: <recognized text>`, real bounds, **no actions**.
- The observation's `screenshot` is set to the captured PNG when the
  caller did not already request one — provenance for the evidence.
- A failed pass (no provider, no window, denied screen recording, Vision
  error) bumps `collection_errors` instead of failing the observation —
  AX content already collected remains valid.

### OCR is evidence, not agency

OCR elements have no semantic actions and no live handle:

- `Target::Element` on an OCR id fails closed:
  `StaleReference("element e_N is OCR-derived — target its bounds center
  as a Point instead")`.
- Acting on them means `Target::Point` at the bounds center → the
  **physical** interaction tier → policy + approval still apply, and it
  stays denied unless the operator enabled `--coords`.
- Digest marks them `[ocr]`; MCP `elements[]` carries `"source": "ocr"`.

## Surface

- CLI: `dexter observe --vision` (composes with `--window`, `--app`).
- MCP: `dexter_observe { vision: true }`.
- `dexter observe` JSON gains `source` on each element.

## Tests

- `crates/vision` unit: bottom-left→top-left mapping, exact 2x Retina
  scale invariance, full-image box = window, PNG IHDR, inert element
  shape (no actions, `source: ocr`).
- `crates/vision` unit: monitor selection (secondary display, spanning /
  off-screen / degenerate → `None`) and monitor-relative pixel crops.
- `crates/vision/tests/ocr.rs` (macOS): real Vision run over a checked-in
  rendered fixture — recognizes "Save document", confidence > 0.5,
  normalized bounds, end-to-end mapping inside the window rect.
- `world-model`: digest emits `[ocr]` only for OCR elements.
- `mcp`: `dexter_observe` accepts `vision: true`.
- `sim`: `vision` is a no-op (no invented elements, no errors).
- Live-verified: `dexter observe --app Finder --window <id> --vision`
  appended 29 `[ocr]` elements (file listing text) beside 6 AX elements.

## Non-goals

- No VLM / cloud vision, no silent fallback on rich AX trees, no
  semantic actions for OCR elements, no OCR on Windows/Linux yet
  (`platform_provider()` → `None` → `collection_errors` bump when asked).
