#!/bin/sh
# Sign the dexter binary with a stable self-signed identity so macOS TCC
# grants (Accessibility, Screen Recording) survive rebuilds.
#
# Why: adhoc signatures change cdhash every build, so a TCC grant to the
# binary is silently dropped (the AX server serves a *degraded* tree —
# `observe` reports ax_limited). A stable "Dexter Dev" certificate gives
# the binary a persistent TCC identity.
#
# One-time setup (creates the self-signed codesigning cert):
#
#   openssl req -x509 -newkey rsa:2048 -keyout /tmp/dexter-dev.key \
#     -out /tmp/dexter-dev.crt -days 3650 -nodes -subj "/CN=Dexter Dev" \
#     -addext "extendedKeyUsage=codeSigning" -addext "keyUsage=digitalSignature"
#   security import /tmp/dexter-dev.crt -k login.keychain-db
#   security import /tmp/dexter-dev.key -k login.keychain-db -t priv -x
#   security add-trusted-cert -p codeSign -k login.keychain-db /tmp/dexter-dev.crt
#     # ^ shows a GUI dialog — approve once
#
# Then sign after every build:
#   ./scripts/devsign.sh            # signs target/debug/dexter
#   ./scripts/devsign.sh release    # signs target/release/dexter

set -eu

PROFILE="${1:-debug}"
BIN="$(dirname "$0")/../target/$PROFILE/dexter"

if [ ! -x "$BIN" ]; then
    echo "no binary at $BIN — run cargo build first" >&2
    exit 1
fi

if ! security find-identity -v -p codesigning | grep -q "Dexter Dev"; then
    echo "missing 'Dexter Dev' codesigning identity — see this script's header" >&2
    exit 1
fi

codesign --force --sign "Dexter Dev" --identifier "com.dexter.cli" "$BIN"
echo "signed $BIN (identity: com.dexter.cli, cert: Dexter Dev)"
echo "next: run '$BIN doctor --request' and approve the TCC prompt once"
