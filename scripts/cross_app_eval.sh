#!/bin/sh
# Cross-app generalization eval — leave-one-app-out (LODO).
#
# For each holdout group: train a checkpoint on every OTHER app/page,
# then `eval matrix` the affected datasets so the holdout row shows
# transfer while the rest show in-domain context.
#
# Usage:
#   scripts/cross_app_eval.sh [holdout ...]
# Default holdouts: the two synthetic domains (cross-surface — the
# informative axis; within-surface app holdouts add little).
# Requires: python3 -m pip install laya (+ torch), cargo-built dexter.
# Trainer runs use --device cpu: concurrent MPS workers stall.

set -eu
cd "$(dirname "$0")/.."

DEX="cargo run -q -p dexter-cu --"
FT="python3 workers/laya/finetune.py"
WORKER="python3 workers/laya/worker.py --provider laya"
ALL="datasets/browser/items.jsonl datasets/macos/items.jsonl datasets/sim/items.jsonl datasets/vision/items.jsonl"

# Rows carry `"app"` provenance (eval export emits it since v0.1).
$DEX eval export $ALL -o /tmp/dexter-rows-all.jsonl

holdouts="${*:-sim/ vision/}"
for app in $holdouts; do
    name=$(echo "$app" | tr '/.' '__' | sed 's/_$//')
    python3 - "$app" <<'PY'
import json, sys
app = sys.argv[1]
rows = [json.loads(l) for l in open('/tmp/dexter-rows-all.jsonl')]
keep = [r for r in rows if not r['app'].startswith(app)]
with open('/tmp/dexter-rows-train.jsonl', 'w') as f:
    for r in keep:
        f.write(json.dumps(r, ensure_ascii=False) + '\n')
print(f"holdout {app}: {len(keep)} train rows")
PY
    $FT /tmp/dexter-rows-train.jsonl --out "/tmp/dexter-ckpt-$name" \
        --epochs 25 --perms 6 --holdout 0.15 --device cpu
    echo "=== holdout: $app ==="
    $DEX eval matrix --engine laya \
        --engine-path "$WORKER --model /tmp/dexter-ckpt-$name --subfolder root" \
        $ALL | grep -E "^datasets|  $app|  \(all\)"
done
