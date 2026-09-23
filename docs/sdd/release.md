# SDD — Release packaging (v0.1.0)

Status: implemented (Item 2 of the maturity plan).

## Decisions

- **Versioning**: single `workspace.package.version = "0.1.0"`; every
  crate uses `version.workspace = true`. `env!("CARGO_PKG_VERSION")`
  propagates to `dexter --version` and the MCP `serverInfo` for free.
- **Signing**: ad-hoc (`codesign --sign - --force --options runtime`).
  There is no Apple Developer certificate in this project — the binary
  is *signed* (integrity, stable-enough cdhash for TCC prompts) but
  **not notarized**: Gatekeeper will block first launch of quarantined
  files. The honest mitigations are documented at install time:
  `brew install --no-quarantine` or `xattr -d com.apple.quarantine`.
- **Shape**: one universal tarball per release —
  `dexter-<ver>-macos-universal.tar.gz` containing `dexter` and
  `dexter-overlay` at the root (what the cask's `binary` stanzas link).
- **Scope**: macOS only, `aarch64` + `x86_64`. Crates.io publishing is
  a non-goal for v0.1.0.

## Pipeline (`.github/workflows/release.yml`)

Triggered by `v*` tags.

1. **build** (matrix, `macos-14`): `cargo build --release --locked`
   for `aarch64-apple-darwin` and `x86_64-apple-darwin`, uploading
   `dexter` + `dexter-overlay` per target. The x86_64 slice is
   cross-compiled on the arm runner — verified locally before tagging.
2. **package**: extract both slices, `lipo -create` into universal
   binaries, ad-hoc `codesign`, `codesign --verify`, tarball +
   `SHA256SUMS`, `gh release create --generate-notes` (tags containing
   `-` become prereleases).
3. **tap**: updates `Casks/dexter.rb` in `Shugar03/homebrew-dexter`
   (version + sha256). Requires the `TAP_GITHUB_TOKEN` secret — the
   default `GITHUB_TOKEN` cannot push outside the repo. When the
   secret is absent the job prints the version/sha pair as a warning
   and the cask is updated by hand.

## Verification

Done locally before tagging:

- `cargo build --release --locked` for both targets ✔
- `lipo` universal + `codesign --verify` satisfies designated
  requirement ✔ (`file` reports both slices)
- `dexter --version` → `0.1.0`, `dexter doctor` runs ✔
- `ruby -c Casks/dexter.rb` → Syntax OK ✔
- Release workflow YAML parses ✔

Still open: first real tag (`v0.1.0-rc.1` then `v0.1.0`) to exercise
the pipeline end-to-end, then fill the cask `sha256` from the
published asset and, if the tap is reachable, `brew audit` /
`brew install` smoke test.

## Honest caveats (user-facing)

- Ad-hoc signing ≠ notarized. `--no-quarantine` or `xattr -d` is
  required; this is documented in the cask `caveats`, the tap README
  and the project README.
- TCC grants (Accessibility, Screen Recording) attach to the signing
  identity — the release build's ad-hoc signature is stable across
  reinstalls of the same artifact, but a new release is a new
  signature: macOS may re-prompt. `dexter doctor` reports the state.
