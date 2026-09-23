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
import re
import signal
import sys

# Die on SIGPIPE like a normal Unix filter instead of raising
# BrokenPipeError at interpreter shutdown when dexter exits first.
signal.signal(signal.SIGPIPE, signal.SIG_DFL)


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


class LayaProvider:
    """Real Laya provider — convaiinnovations/laya checkpoints.

    The model evaluates every question in ONE forward pass, so we batch
    the whole request's questions into a single predict() call.

    Wire mapping (ours -> laya):
      choice{id,prompt,options} -> choice{instructions,criteria:{i:opt}}
      score{id,prompt}          -> score{instructions,criteria:[bins]}
      bool{id,prompt}           -> noul{instructions}

    Answer mapping (laya -> ours): choice returns the criteria key (we
    emit "0","1",..), noul returns a probability we threshold at 0.5,
    score returns a float we clamp to [0,1].
    """

    SCORE_BINS = ["0.0", "0.25", "0.5", "0.75", "1.0"]

    def __init__(self, model: str, subfolder: str | None, device: str | None):
        import laya

        self.agent = laya.load(model, subfolder=subfolder, device=device)
        # Warm the tokenizer + first forward pass — the first real
        # predict otherwise pays seconds of lazy init.
        self.agent.predict("warmup", {
            "w": {"type": "choice", "instructions": "pick one",
                  "criteria": {"opt0": "a", "opt1": "b"}},
        })

    def predict(self, state: str, questions: list) -> list:
        laya_qs, order = {}, []
        for q in questions:
            qid = q["id"]
            order.append((qid, q["type"]))
            if q["type"] == "choice":
                # Criteria keys are rendered to the model as "key: text".
                # c{i} = action candidate, r{i} = route option — the key
                # itself carries the option type. Strip the redundant
                # "candidate i: " prefix our engine prepends.
                criteria = {}
                for i, opt in enumerate(q["options"]):
                    if re.match(r"^candidate \d+:", opt):
                        criteria[f"c{i}"] = re.sub(r"^candidate \d+:\s*", "", opt)
                    else:
                        criteria[f"r{i}"] = opt
                laya_qs[qid] = {
                    "type": "choice",
                    "instructions": q["prompt"],
                    "criteria": criteria,
                }
            elif q["type"] == "score":
                laya_qs[qid] = {
                    "type": "score",
                    "instructions": q["prompt"],
                    "criteria": self.SCORE_BINS,
                }
            else:  # bool -> noul
                laya_qs[qid] = {"type": "noul", "instructions": q["prompt"]}

        res = self.agent.predict(state, laya_qs)
        out = []
        for qid, qtype in order:
            a = res["answers"][qid]
            if qtype == "choice":
                # choice comes back as the criteria key ("c3" / "r5")
                m = re.match(r"[cr](\d+)$", str(a["choice"]))
                if m is None:
                    raise ValueError(f"unparseable choice key: {a['choice']!r}")
                idx = int(m.group(1))
                out.append({
                    "type": "choice", "id": qid, "index": idx,
                    # calibrated P(this pick is correct) — engines may
                    # gate acting on it
                    "confidence": a.get("confidence"),
                })
            elif qtype == "score":
                out.append({"type": "score", "id": qid, "value": max(0.0, min(1.0, float(a["score"])))})
            else:
                out.append({"type": "bool", "id": qid, "value": bool(a["noul"] >= 0.5)})
        return out


def handle(req: dict, provider) -> dict:
    params = req.get("params", {})
    state = params.get("state", "")
    questions = params.get("questions", [])
    if provider == "dev":
        answers = [answer_dev(q, state) for q in questions]
        name = "dev"
    else:
        answers = provider.predict(state, questions)
        name = "laya"
    return {"id": req.get("id"), "ok": True, "provider": name, "answers": answers}


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--provider", default="dev", choices=["dev", "laya"])
    ap.add_argument("--model", default="convaiinnovations/laya",
                    help="HF model id (default: laya family repo)")
    ap.add_argument("--subfolder", default="multilingual",
                    help="checkpoint subfolder: multilingual (localized UIs) | "
                         "typed-decisions | root for the english checkpoint")
    ap.add_argument("--device", default=None, help="cpu | cuda | mps (default: auto)")
    args = ap.parse_args()

    provider = "dev"
    if args.provider == "laya":
        # Load once, fail fast at startup rather than mid-session.
        try:
            subfolder = None if args.subfolder in ("", "root") else args.subfolder
            provider = LayaProvider(args.model, subfolder, args.device)
        except Exception as e:
            print(json.dumps({
                "id": None, "ok": False,
                "error": f"laya provider failed to load: {e}",
            }), flush=True)
            return 2

    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
            resp = handle(req, provider)
        except Exception as e:  # never crash the protocol loop
            rid = None
            try:
                rid = json.loads(line).get("id")
            except Exception:
                pass
            resp = {"id": rid, "ok": False, "error": str(e)}
        sys.stdout.write(json.dumps(resp) + "\n")
        try:
            sys.stdout.flush()
        except BrokenPipeError:
            return 0
    return 0


if __name__ == "__main__":
    sys.exit(main())
