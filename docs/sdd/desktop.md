# SDD: desktop — journal, policy, approvals, and read-only sessions

Etapa 6 begins with a Tauri 2 shell that observes runtime state without
adding a general runtime control surface.

**Contract**

- `dexter-desktop` exposes `snapshot`, the narrowly scoped `approve` and
  `deny` commands, and read-only `sessions` and `open_session` commands.
  It polls a shared `dexter-core::JournalTail` from byte 0, defers
  partial JSONL lines, and retains the newest 200 events.
- Pending approval requests are derived from every tailed event and kept
  separately from the capped event ring. A decision stays visible as
  sent until the journal confirms it.
- Policy is loaded once at startup. Missing configuration uses the
  embedded policy; a configured policy load error is displayed as an
  error and never replaced with the embedded default. The view reports
  ordered rules, effective defaults, and whether the physical deny floor
  is implicit.
- The UI polls every 500 ms and renders state as text. Approve/deny are
  the only commands that write to the runtime; closing the desktop writes
  nothing.
- `--events <path>` configures the journal and `--policy <path>`
  configures the policy file. `--approvals <path>` opts into the
  approval inbox. `--sessions <dir>` opts into the local session list;
  without it, sessions are `NotConfigured` and the existing behavior is
  unchanged.

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

## Sessions

`--sessions <dir>` enables a non-recursive listing of regular,
non-symlink `*.jsonl` journals and `*.json` recordings. Entries are
ordered by modification time descending and name, capped at 200, with
truncation reported explicitly. A missing or unreadable directory is
shown as an error rather than panicking.

`open_session` accepts a bare listed name, rechecks that it is a
regular, non-symlink file in the configured directory, and refuses
files larger than 16 MiB. Recordings are parsed as a complete
`dexter_eval::recording::Recording`; malformed data is an error, never a
partial step list. Journals are parsed by
`dexter_eval::recording::events_from_jsonl` with the same all-or-error
behavior and shown as an event timeline, not converted into a recording.
The read-only UI steps through one recording step or journal event at a
time. It does not execute actions or replay sessions. Re-execution stays
with `dexter replay`, which goes through `Engine::run_step`.

Linux builds require `pkg-config`, `libwebkit2gtk-4.1-dev`,
`libgtk-3-dev`, `libsoup-3.0-dev`, `libjavascriptcoregtk-4.1-dev`,
`librsvg2-dev`, and `libayatana-appindicator3-dev`.
