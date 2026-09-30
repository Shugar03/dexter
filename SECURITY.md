# Security model

Dexter is software that acts on a real computer on behalf of a model.
The security posture is: **the model proposes; the runtime disposes.**

## What the runtime guarantees

- **Policy outside the LLM.** Every action — regardless of which
  decision engine, agent or MCP client proposed it — passes through the
  TOML policy engine. Rules are evaluated in order; the default for
  mutating actions is `require_approval`, never allow.
- **Scoped, single-use approvals.** Approvals are fingerprints of the
  *exact* serialized action plus app context, consumed once, and expire
  after a TTL. Approving "click Save in TextEdit" does not approve
  clicking anything else, in any other app, later. A grant additionally
  only answers a live pending request: the engine records every
  `needs_approval` escalation, and fingerprints that were never
  escalated are rejected — approval cannot be minted for an action the
  runtime never proposed.
- **Coordinates are opt-in.** Coordinate-level input (`point:x,y`,
  physical keyboard injection) only reaches the driver when the caller
  explicitly sets `allow_coordinates`. There is no silent fallback from
  a semantic target to screen pixels.
- **Fail-closed targets.** Ambiguous semantic matches and stale element
  references are errors, not guesses.
- **No simulated success.** An action reports `Success` only when the
  driver performed it; expected effects are verified against fresh
  observations. `UNCERTAIN` never counts as verified.
- **Honest perception.** Degraded Accessibility trees are flagged
  (`ax_limited`) and truncate-dependent verifications to `UNCERTAIN`.
- **Audit.** Every observation, policy check, decision, action and
  verification lands in a structured event journal (JSONL).
- **Secrets.** `AXSecureTextField` values are redacted during
  collection — password contents never enter the process. The browser
  walker applies the same rule: `input[type=password]` values are
  nulled inside the page before serialization.

## What it does NOT guarantee (yet)

- The Accessibility API itself is trusted input: a malicious app could
  craft a misleading AX tree. Mitigations: policy scoping per app,
  verification against fresh state, coordinates disabled by default.
  Stronger isolation (per-driver sandboxing) is on the roadmap.
- Approvals granted via `dexter_grant`/`--approve` trust whoever runs
  them — there is no cryptographic binding to a human identity yet. The
  pending-request binding proves a grant answers a real escalation, but
  not *who* answered it: deployments that must keep the grant out of the
  agent's reach should not expose `dexter_grant` to the agent at all
  (tool filtering or an approval proxy).
- The dev Laya provider is a deterministic heuristic for protocol
  development, not a safety component.

## Reporting

Open a private GitHub security advisory, or email the maintainer listed
in `Cargo.toml`. Please include repro steps and whether the issue lets
an action bypass policy, fake a verification, or leak secret contents.
