# SDD: permissions — doctor onboarding + system prompt

The macOS driver needs two TCC permissions before the ODAV loop can do
anything: Accessibility (AX trees, synthetic input) and Screen Recording
(window titles, pixel capture). Before this slice, `doctor` reported
raw booleans and `--request` poked both TCC entry points
unconditionally — a missing grant surfaced downstream as an opaque
driver failure instead of a guided fix.

## Contract

- `permissions::Permission` — the closed set the driver requires:
  `Accessibility`, `ScreenRecording`. `Permission::ALL` is the check
  order and the doctor display order. Each variant carries its own
  metadata as data: `name()`, `enables()` (the capability it gates),
  `remediation()` (exact System Settings pane + toggle for a manual
  grant — Ventura+: Privacy & Security).
- `permissions::Probe` — the OS seam: `granted(p)` reads TCC state,
  `request(p)` triggers the system prompt for `p` and reports the
  post-prompt state. `permissions::System` is the real TCC-backed
  impl (`AXIsProcessTrustedWithOptions` prompt key /
  `CGRequestScreenCaptureAccess`). Tests inject a fake.
- `permissions::check(&probe) -> Vec<Status>` — one `Status`
  (`{permission, granted}`) per `Permission::ALL`.
- `permissions::request_missing(&probe, &[Status]) -> Vec<Status>` —
  prompts **only** the permissions currently missing, then re-reads
  state and returns fresh statuses. Never prompts a granted
  permission: `--request` is a fix flow, not a poke.
- `dexter doctor` macOS section renders one row per permission:
  `granted`/`missing` + what it enables; every `missing` row is
  followed by its `remediation` line and ends with a summary verdict
  naming the next step (`doctor --request` or the Settings path).
- `--request` then re-renders the post-prompt state — a permission
  the user just granted shows `granted` without a restart.

## Honesty rules (unchanged)

- Fail-closed: status reflects what TCC reports *now*; doctor never
  asserts a grant it can't re-read.
- The prompt path is TCC's, not ours: `CGRequestScreenCaptureAccess`
  only shows its dialog on the first unanswered call per install —
  if macOS swallows the prompt, the `missing` row + remediation is
  still correct and actionable (`tccutil reset` is documented, not
  invoked).
- Accessibility trust attributes to the responsible process: a dexter
  launched from Terminal may need the *terminal* granted, not the
  binary — the existing `ax_limited` note stays.

## Tests

Hermetic at the `Probe` seam (`FakeProbe` scripted per-permission,
recording requests): `check` covers every `Permission::ALL` in order;
`request_missing` prompts missing-only and returns post-prompt state;
every permission's metadata is non-empty (regression guard against a
blank remediation). The real prompt is verified manually on macOS —
TCC can't be unit-tested.
