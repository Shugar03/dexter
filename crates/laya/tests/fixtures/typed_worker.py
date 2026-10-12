"""Protocol stub for typed questions: answers choices and bools.

Usage: typed_worker.py <pick_index> <blocking:0|1> <disambiguate_index> [confidence]

TYPED_BOOLS may override bool answers by question id; otherwise bools
use the existing blocking argv value. TYPED_OMIT_BOOL can omit named
bool answers to test fail-closed protocol handling.
"""
import json
import os
import sys

PICK = int(sys.argv[1])
BLOCKING = sys.argv[2] == "1"
DISAMBIG = int(sys.argv[3])
CONF = float(sys.argv[4]) if len(sys.argv) > 4 else 0.9
BOOL_OVERRIDES = json.loads(os.environ.get("TYPED_BOOLS", "{}"))
OMIT_BOOLS = set(filter(None, os.environ.get("TYPED_OMIT_BOOL", "").split(",")))

for line in sys.stdin:
    req = json.loads(line)
    if req.get("method") == "hello":
        sys.stdout.write(
            json.dumps({"id": req["id"], "ok": True, "protocol": 1, "worker": "typed"})
            + "\n"
        )
        sys.stdout.flush()
        continue
    answers = []
    for q in req.get("params", {}).get("questions", []):
        if q["type"] == "bool":
            if q["id"] not in OMIT_BOOLS:
                answers.append(
                    {
                        "type": "bool",
                        "id": q["id"],
                        "value": BOOL_OVERRIDES.get(q["id"], BLOCKING),
                    }
                )
        else:
            idx = DISAMBIG if q["id"] == "disambiguate" else PICK
            answers.append(
                {"type": "choice", "id": q["id"], "index": idx, "confidence": CONF}
            )
    sys.stdout.write(
        json.dumps({"id": req["id"], "ok": True, "provider": "typed", "answers": answers})
        + "\n"
    )
    sys.stdout.flush()
