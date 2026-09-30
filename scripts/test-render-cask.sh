#!/usr/bin/env bash
# Golden test for scripts/render-cask.sh — the release `tap` job renders
# the Homebrew cask from packaging/homebrew/dexter.rb.in, so the template
# must reproduce the published cask exactly and reject malformed inputs.
set -euo pipefail
cd "$(dirname "$0")/.."

render=scripts/render-cask.sh
golden=packaging/homebrew/testdata/dexter-0.1.0-rc.1.rb
sha=7a9dbe24fd21d3a5d285b09448a2cb663fcd10c5a7e0d8b13823c952ae173cde
out=$(mktemp)
trap 'rm -f "$out"' EXIT

"$render" 0.1.0-rc.1 "$sha" > "$out"
diff -u "$golden" "$out"
echo "ok: template reproduces the published 0.1.0-rc.1 cask"

# A leading `v` is stripped (tags are v-prefixed; the cask is not).
"$render" v0.1.0-rc.1 "$sha" | diff -u "$golden" -
echo "ok: v-prefixed tag accepted"

reject() {
  if "$render" "$@" > /dev/null 2>&1; then
    echo "FAIL: accepted invalid input: $*" >&2
    exit 1
  fi
  echo "ok: rejected $*"
}
reject
reject 0.1.0
reject "" "$sha"
reject 0.1.0 ""
reject 0.1.0 "${sha:0:63}"
reject 0.1.0 "${sha^^}"
reject '0.1.0"; system("x")' "$sha"
reject 0.1.0 "$sha" extra

if command -v ruby > /dev/null; then
  ruby -c "$out"
fi
