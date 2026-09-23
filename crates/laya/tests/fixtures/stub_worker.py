"""Protocol stub: answers every question with a fixed choice index and
confidence taken from argv — lets tests drive the abstention gate
without a model.

Usage: stub_worker.py [confidence] [index]
"""
import json
import sys

CONF = float(sys.argv[1]) if len(sys.argv) > 1 else 0.5
IDX = int(sys.argv[2]) if len(sys.argv) > 2 else 0

for line in sys.stdin:
    req = json.loads(line)
    answers = [
        {"type": "choice", "id": q["id"], "index": IDX, "confidence": CONF}
        for q in req.get("params", {}).get("questions", [])
    ]
    sys.stdout.write(
        json.dumps({"id": req["id"], "ok": True, "provider": "stub", "answers": answers})
        + "\n"
    )
    sys.stdout.flush()
