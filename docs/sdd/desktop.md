# SDD: desktop — read-only journal and policy scaffold

Etapa 6 begins with a Tauri 2 shell that observes runtime state without
adding a control surface.

**Contract**

- `dexter-desktop` exposes exactly one Tauri command, `snapshot`. It
  polls a shared `dexter-core::JournalTail` from byte 0, defers partial
  JSONL lines, and retains the newest 200 events.
- Policy is loaded once at startup. Missing configuration uses the
  embedded policy; a configured policy load error is displayed as an
  error and never replaced with the embedded default. The view reports
  ordered rules, effective defaults, and whether the physical deny floor
  is implicit.
- The UI polls every 500 ms and renders state as text. There are no
  approvals, run, edit, or other control operations in this slice.
- `--events <path>` configures the journal and `--policy <path>`
  configures the policy file.

Linux builds require `pkg-config`, `libwebkit2gtk-4.1-dev`,
`libgtk-3-dev`, `libsoup-3.0-dev`, `libjavascriptcoregtk-4.1-dev`,
`librsvg2-dev`, and `libayatana-appindicator3-dev`.
