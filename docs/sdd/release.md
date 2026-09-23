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
  files — verified live: the cask-installed binary carried
  `com.apple.quarantine` and hung until cleared. The documented
  mitigation is `xattr -d com.apple.quarantine` (works; measured).
  `--no-quarantine` is NOT a valid `brew install` flag on current
  Homebrew — earlier docs claimed it; corrected.
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

Verified end-to-end on `v0.1.0-rc.1` (run 35875691047):

- Matrix builds + `lipo` + ad-hoc sign + `SHA256SUMS` ✔
- `gh release create` needed `--repo` (package job has no checkout) —
  fixed after the first run failed on exactly that ✔
- Prerelease published with both assets ✔
- `brew install --cask shugar03/dexter/dexter` installs and links both
  binaries ✔; `xattr -d` clears quarantine and `dexter doctor` runs ✔
- `tap` job needs the `TAP_GITHUB_TOKEN` secret — absent, so the cask
  was updated by hand (version + real sha256 of the RC asset).
- Cask `depends_on macos:` modernized to `:ventura` (the `">= :"` string
  form is deprecated — `brew` flagged it on install).

## Honest caveats (user-facing)

- Ad-hoc signing ≠ notarized. `xattr -d com.apple.quarantine` is
  required once after install; documented in the cask `caveats` and
  the project README. (`--no-quarantine` was removed from Homebrew —
  do not document it as an option.)
- TCC grants (Accessibility, Screen Recording) attach to the signing
  identity — the release build's ad-hoc signature is stable across
  reinstalls of the same artifact, but a new release is a new
  signature: macOS may re-prompt. `dexter doctor` reports the state.
