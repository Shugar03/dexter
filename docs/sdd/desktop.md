# SDD: desktop — journal, policy, and approval inbox

Etapa 6 begins with a Tauri 2 shell that observes runtime state without
adding a general runtime control surface.

**Contract**

- `dexter-desktop` exposes `snapshot` plus the narrowly scoped `approve`
  and `deny` commands. It polls a shared `dexter-core::JournalTail` from
  byte 0, defers partial JSONL lines, and retains the newest 200 events.
- Pending approval requests are derived from every tailed event and kept
  separately from the capped event ring. A decision stays visible as
  sent until the journal confirms it.
- Policy is loaded once at startup. Missing configuration uses the
  embedded policy; a configured policy load error is displayed as an
  error and never replaced with the embedded default. The view reports
  ordered rules, effective defaults, and whether the physical deny floor
  is implicit.
- The UI polls every 500 ms and renders state as text. Approve/deny are
  the only control operations; closing the desktop writes nothing.
- `--events <path>` configures the journal and `--policy <path>`
  configures the policy file. `--approvals <path>` opts into the
  approval inbox.

## Approvals

The desktop and engine are separate processes with no cross-process
approval channel. The append-only inbox provides that channel while
feeding the existing `Engine::approve_pending` / `deny_pending` path;
it is not a second grant mechanism. Both ends opt in: start the desktop
with `--approvals <path>` and Dexter with
`--approval-inbox <same path>`. With the desktop channel disabled,
pending requests are shown without decision buttons.

The inbox carries the same trust as `dexter_grant`, which is already
agent-reachable. It therefore only answers live requests, is
fingerprint-bound, single-use, TTL-limited, and journaled. On Unix,
group/world-writable inbox files are rejected. The desktop creates a
missing inbox with mode `0600` and appends with `O_APPEND`; it never
pre-grants. Closing without a decision writes nothing, so the step
remains unapproved (deny by default on close).

Each complete newline-terminated JSONL record has this shape:

```json
{"fingerprint":"…","decision":"grant","ts_ms":0}
```

`decision` is `grant` or `deny`. The engine ignores bytes present before
inbox attachment and defers an incomplete trailing line until its
newline arrives.

Linux builds require `pkg-config`, `libwebkit2gtk-4.1-dev`,
`libgtk-3-dev`, `libsoup-3.0-dev`, `libjavascriptcoregtk-4.1-dev`,
`librsvg2-dev`, and `libayatana-appindicator3-dev`.
