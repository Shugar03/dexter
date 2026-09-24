# SDD: Agent Contract v2

## Purpose

CLI, MCP and SDKs must expose one coherent, round-trippable and privacy-safe
runtime contract. V2 accepts v1 input for one minor release and emits v2.

## Versioning

- MCP status and tool responses include `contract_version: 2`.
- Event records include `schema_version: 2` with a serde default of 1.
- Parsers accept v1 and v2 during the migration window.
- Deprecation warnings are operator-facing; tool calls remain machine-readable.

## Element and observation references

`ElementId` accepts both JSON forms:

```json
4
"e_4"
```

V2 emits `"e_4"`. `ObservationId` remains numeric. A concrete action target is:

```json
{"observation":12,"element":"e_4"}
```

Candidates generated from an observation use this form instead of reducing the
target to role/name. Drivers revalidate identity at execute time.

`GenHistory` stores structured attempt records: requested action, stable target
descriptor, outcome and error. Repeat penalties and arithmetic sequencing do
not recover labels by parsing an action target.

## Observe response

`dexter_observe` v2 returns:

```json
{
  "contract_version": 2,
  "observation": 12,
  "windows": [],
  "element_count": 900,
  "elements_returned": 500,
  "elements_output_truncated": true,
  "elements_truncated": false,
  "collection_errors": 0,
  "ax_limited": false,
  "screenshot": "/private/tmp/dexter-obs-12.png",
  "delta": null,
  "digest": "...",
  "elements": []
}
```

- Source-tree truncation and response-output truncation are separate.
- MCP accepts `screenshot: true`, but never an arbitrary output path; Dexter
  chooses a temp path.
- `since: ObservationId` optionally returns `ObservationDelta` from a bounded
  Engine cache: added, removed, changed, focus/window changes and completeness.
- `window` and `vision` compose consistently across observe, candidates,
  verify and task.

## Audit versus training capture

The public journal is always an audit stream and always redacted.

- Action summaries include kind, target metadata, mechanism, tier, sensitivity
  and payload length/hash, never payload text.
- Candidate/decision audit events omit state digest, goal literals and free-form
  rationale.
- Clipboard, secure fields and typed/set values never appear in audit events,
  overlay labels or errors.
- Fingerprints are `sha256:<hex>` over canonical authorization input, never
  serialized action JSON.

Eval needs complete curated decision contexts. Engine may enable a separate,
private `TrainingCapture` buffer for eval; MCP never exposes it. The existing
v1 `rows_from_events` reader remains for old journals, while v2 eval export
uses training captures. Clipboard and secure-field contents are excluded even
from training capture.

## MCP outcomes

`dexter_act` and `dexter_task` return typed statuses consistently:

- `done` / `completed`
- `needs_approval { fingerprint, reason }`
- `denied { reason }`
- `abstained`
- `escalated`
- `failed`
- `cancelled`
- `timed_out`

A task awaiting approval returns and releases the engine. It does not block a
concurrent `dexter_grant` call.

## Capability report

`dexter_status` replaces coarse booleans-only output with a structured action
matrix while preserving v1 fields:

- supported action kinds;
- mechanisms;
- app lifecycle/window/clipboard support;
- physical fallback availability;
- screenshots/vision;
- decision-engine health.

Dynamic app affordances still come from `dexter_map`/`dexter_observe`.

## Python SDK parity

Every MCP tool and relevant parameter has a typed wrapper: map, observe
(window/vision/screenshot/since), candidates (app/window/vision/max), act
(app/expect), verify (app/window/vision), task (app/bounds/perception), grant,
cancel, journal and status.

## TDD contract

1. V1 numeric and v2 string element ids both deserialize; v2 output
   round-trips into `dexter_act`.
2. Output truncation is explicit and independent of source truncation.
3. Screenshot output uses a Dexter-owned temp path.
4. Journal serialization cannot find a typed secret sentinel.
5. Training capture still produces eval rows without exposing private data via
   `dexter_journal`.
6. Task approval response allows grant and successful re-invocation.
7. SDK fake-server tests cover every public method and parameter.
8. Frozen v1 datasets and journals continue to load.
