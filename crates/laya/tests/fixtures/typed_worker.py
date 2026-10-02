"""Protocol stub for typed questions: answers `pick` and `disambiguate`
choices and the `blocking_modal` bool with values taken from argv.

Usage: typed_worker.py <pick_index> <blocking:0|1> <disambiguate_index> [confidence]
"""
import json
import sys

PICK = int(sys.argv[1])
BLOCKING = sys.argv[2] == "1"
DISAMBIG = int(sys.argv[3])
CONF = float(sys.argv[4]) if len(sys.argv) > 4 else 0.9

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
            answers.append({"type": "bool", "id": q["id"], "value": BLOCKING})
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
