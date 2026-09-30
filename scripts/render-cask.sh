#!/usr/bin/env bash
# Render the Homebrew cask for a release to stdout.
#   scripts/render-cask.sh <version|vVERSION> <sha256>
# Single source of truth: packaging/homebrew/dexter.rb.in. Inputs are
# validated strictly — a malformed version or digest fails the release
# instead of publishing a broken cask.
set -euo pipefail

if [ "$#" -ne 2 ]; then
  echo "usage: $0 <version> <sha256>" >&2
  exit 2
fi

version=${1#v}
sha=$2

if ! [[ $version =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$ ]]; then
  echo "render-cask: invalid version: '$1'" >&2
  exit 2
fi
if ! [[ $sha =~ ^[0-9a-f]{64}$ ]]; then
  echo "render-cask: invalid sha256: '$sha'" >&2
  exit 2
fi

template="$(dirname "$0")/../packaging/homebrew/dexter.rb.in"
sed -e "s/@VERSION@/$version/" -e "s/@SHA256@/$sha/" "$template"
