#!/usr/bin/env python3
"""Dexter Laya sidecar — NDJSON predict protocol over stdio.

Protocol (one JSON object per line, stdin -> stdout):

    -> {"id": 1, "method": "predict",
        "params": {"state": "...", "questions": [Question...]}}
    <- {"id": 1, "ok": true, "provider": "dev", "answers": [Answer...]}
    <- {"id": 1, "ok": false, "error": "..."}

Question/Answer shapes mirror crates/decision (`type: choice|score|bool`).

Providers (`--provider`, default `dev`):
  dev   — deterministic keyword heuristic for development and contract
          tests. Labeled "dev" in every response; NOT a decision model.
  laya  — the real Laya model. Fails honestly when the SDK/checkpoint
          is not installed: this worker exits with an error line and
          dexter reports DecisionError, never a silent fallback.
"""
import argparse
import json
import sys


def answer_dev(question: dict, state: str) -> dict:
    """Deterministic dev heuristic — keyword overlap between option text
    and the [GOAL] section of the state. Purely for protocol development.
    """
    qtype = question.get("type")
    if qtype == "bool":
        return {"type": "bool", "id": question["id"], "value": True}
    if qtype == "score":
        return {"type": "score", "id": question["id"], "value": 0.5}
    # choice: score each option by keyword overlap with the goal.
    goal = ""
    marker = "[GOAL]"
    if marker in state:
        goal = state.split(marker, 1)[1].split("[", 1)[0].lower()
    words = {w for w in goal.replace("/", " ").split() if len(w) >= 3}
    best, best_score = 0, -1.0
    for i, opt in enumerate(question.get("options", [])):
        text = opt.lower()
        score = sum(1 for w in words if w in text)
        # Prefer real candidates over trailing route options when tied.
        if score > best_score:
            best, best_score = i, float(score)
    return {"type": "choice", "id": question["id"], "index": best}


def predict_laya(state: str, questions: list) -> list:
    """Real Laya provider — requires the laya SDK + checkpoint."""
    try:
        import laya  # noqa: F401  (SDK is not public yet)
    except ImportError as e:
        raise RuntimeError(
            "laya SDK not installed — run with --provider dev for protocol "
            "development, or install the laya package + checkpoint"
        ) from e
    raise RuntimeError("laya provider: SDK wiring not implemented yet")


def handle(req: dict, provider: str) -> dict:
    params = req.get("params", {})
    state = params.get("state", "")
    questions = params.get("questions", [])
    answers = []
    for q in questions:
        if provider == "laya":
            answers.extend(predict_laya(state, [q]))
        else:
            answers.append(answer_dev(q, state))
    return {
        "id": req.get("id"),
        "ok": True,
        "provider": provider,
        "answers": answers,
    }


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--provider", default="dev", choices=["dev", "laya"])
    args = ap.parse_args()

    if args.provider == "laya":
        # Fail fast at startup rather than mid-session.
        try:
            import laya  # noqa: F401
        except ImportError:
            print(json.dumps({
                "id": None, "ok": False,
                "error": "laya SDK not installed",
            }), flush=True)
            return 2

    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
            resp = handle(req, args.provider)
        except Exception as e:  # never crash the protocol loop
            rid = None
            try:
                rid = json.loads(line).get("id")
            except Exception:
                pass
            resp = {"id": rid, "ok": False, "error": str(e)}
        sys.stdout.write(json.dumps(resp) + "\n")
        sys.stdout.flush()
    return 0


if __name__ == "__main__":
    sys.exit(main())
