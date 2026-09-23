"""Crash stub: answers exactly one request then exits — exercises the
engine's respawn+retry supervision.

Usage: die_once.py [index]
"""
import json
import sys

IDX = int(sys.argv[1]) if len(sys.argv) > 1 else 0

line = sys.stdin.readline()
if line:
    req = json.loads(line)
    answers = [
        {"type": "choice", "id": q["id"], "index": IDX, "confidence": 0.9}
        for q in req.get("params", {}).get("questions", [])
    ]
    sys.stdout.write(
        json.dumps({"id": req["id"], "ok": True, "provider": "stub", "answers": answers})
        + "\n"
    )
    sys.stdout.flush()
sys.exit(0)
