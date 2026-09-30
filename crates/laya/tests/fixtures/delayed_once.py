"""Protocol stub: delays its FIRST response by argv ms, then answers
instantly. Exercises stale-line handling — a request that times out
still queues its (late) response on the channel; the engine must drop
it by id, not consume it positionally.

Usage: delayed_once.py [delay_ms]
"""
import json
import sys
import time

DELAY = float(sys.argv[1]) / 1000.0 if len(sys.argv) > 1 else 0.3

first = True
for line in sys.stdin:
    req = json.loads(line)
    if first:
        first = False
        time.sleep(DELAY)
    answers = [
        {"type": "choice", "id": q["id"], "index": 0}
        for q in req.get("params", {}).get("questions", [])
    ]
    sys.stdout.write(
        json.dumps({"id": req["id"], "ok": True, "provider": "stub", "answers": answers})
        + "\n"
    )
    sys.stdout.flush()
