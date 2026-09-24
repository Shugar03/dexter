# Security model

Dexter is software that acts on a real computer on behalf of a model.
The security posture is: **the model proposes; the runtime disposes.**

## What the runtime guarantees

- **Policy outside the LLM.** Every action — regardless of which
  decision engine, agent or MCP client proposed it — passes through the
  TOML policy engine. Rules are evaluated in order; the default for
  mutating actions is `require_approval`, never allow.
- **Scoped, single-use approvals.** Approvals are SHA-bound fingerprints
  of the *exact* serialized route — action kind, mechanism, tier,
  sensitivity, resolved target identity and non-secret parameters —
  consumed once, and expire after a TTL. Approving "click Save in
  TextEdit" does not approve clicking anything else, in any other app,
  or through a different mechanism, later.
- **Visible side effects are authorized.** Foregrounding an app is a
  policy-evaluated stage borrow, not a precondition: a `deny` on
  `launch_app` refuses every activation — step wakes, `dexter_map`
  probes and eval borrows included — and an unapproved one surfaces its
  own fingerprint. No code path calls `driver.wake` unauthenticated.
- **Agents cannot self-serve approvals.** `dexter mcp --no-grants`
  removes the `dexter_grant` tool, so the channel that returns a
  `needs_approval` fingerprint cannot also grant it; approvals must
  arrive out of band.
- **Coordinates are opt-in.** Coordinate-level input (`point:x,y`,
  physical keyboard injection) only reaches the driver when the caller
  explicitly sets `allow_coordinates`. There is no silent fallback from
  a semantic target to screen pixels.
- **Fail-closed targets.** Ambiguous semantic matches and stale element
  references are errors, not guesses.
- **Fail-closed policy files.** An unknown or typo'd TOML key is a load
  error listing the valid fields — a malformed rule can never silently
  match more than it says.
- **No simulated success.** An action reports `Success` only when the
  driver performed it; expected effects are verified against fresh
  observations. `UNCERTAIN` never counts as verified.
- **Honest perception.** Degraded Accessibility trees are flagged
  (`ax_limited`) and truncate-dependent verifications to `UNCERTAIN`.
- **Audit.** Every observation, policy check, decision, action and
  verification lands in a structured event journal (JSONL).
- **Secrets.** `AXSecureTextField` values are redacted during
  collection — password contents never enter the process.

## What it does NOT guarantee (yet)

- The Accessibility API itself is trusted input: a malicious app could
  craft a misleading AX tree. Mitigations: policy scoping per app,
  verification against fresh state, coordinates disabled by default.
  Stronger isolation (per-driver sandboxing) is on the roadmap.
- Approvals granted via `dexter_grant`/`--approve` trust whoever runs
  them — there is no cryptographic binding to a human identity yet.
- The dev Laya provider is a deterministic heuristic for protocol
  development, not a safety component.

## Reporting

Open a private GitHub security advisory, or email the maintainer listed
in `Cargo.toml`. Please include repro steps and whether the issue lets
an action bypass policy, fake a verification, or leak secret contents.
