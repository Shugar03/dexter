"""Legacy stub: pre-versioning worker — answers every request `ok`
with answers but never a `protocol` field (it doesn't know `hello`).
The engine must refuse to spawn against it.
"""
import json
import sys

for line in sys.stdin:
    req = json.loads(line)
    answers = [
        {"type": "choice", "id": q["id"], "index": 0}
        for q in req.get("params", {}).get("questions", [])
    ]
    sys.stdout.write(
        json.dumps({"id": req["id"], "ok": True, "provider": "stub", "answers": answers})
        + "\n"
    )
    sys.stdout.flush()
