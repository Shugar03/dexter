#!/usr/bin/env python3
"""Dexter Laya sidecar — NDJSON predict protocol over stdio.

Protocol (one JSON object per line, stdin -> stdout):

    -> {"id": 0, "method": "hello", "params": {"protocol": 1}}
    <- {"id": 0, "ok": true, "protocol": 1, "worker": "laya-worker"}
    -> {"id": 1, "method": "predict", "v": 1,
        "params": {"state": "...", "questions": [Question...]}}
    <- {"id": 1, "ok": true, "provider": "dev", "answers": [Answer...]}
    <- {"id": 1, "ok": false, "error": "..."}

The engine sends `hello` at spawn; a worker that answers `ok` without a
matching `protocol` field (a pre-versioning build) is rejected with an
`ok:false`… or the engine errors — spawning against a stale worker is
never allowed to proceed silently.

Question/Answer shapes mirror crates/decision (`type: choice|score|bool`).

Providers (`--provider`, default `dev`):
  dev      — deterministic keyword heuristic for development and
             contract tests. Labeled "dev" in every response; NOT a
             decision model.
  laya     — the real Laya model (PyTorch SDK). Fails honestly when
             the SDK/checkpoint is not installed: this worker exits
             with an error line and dexter reports DecisionError,
             never a silent fallback.
  laya-mlx — same checkpoints through the MLX port (`pip install
             laya-mlx`, Apple silicon). Labeled "laya-mlx" so audit
             lines name the runtime that answered.
"""
import argparse
import json
import re
import signal
import sys

# Die on SIGPIPE like a normal Unix filter instead of raising
# BrokenPipeError at interpreter shutdown when dexter exits first.
# POSIX-only signal — absent on Windows, where pipes are not signaled.
if hasattr(signal, "SIGPIPE"):
    signal.signal(signal.SIGPIPE, signal.SIG_DFL)

# Protocol revision this worker speaks — bumped when request/response
# shapes change incompatibly. The engine checks it on `hello`.
PROTOCOL = 1


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


class DevProvider:
    name = "dev"

    def predict(self, state: str, questions: list) -> list:
        return [answer_dev(q, state) for q in questions]


class LayaProvider:
    """Real Laya provider — convaiinnovations/laya checkpoints.

    `backend` selects the SDK: "laya" = PyTorch package, "laya-mlx" =
    MLX port (Apple silicon). Both expose the same
    `load(model, subfolder, device)` + `agent.predict` surface, and
    both fail at construction when the SDK or checkpoint is missing —
    the worker reports that error honestly and exits.

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

    def __init__(self, model: str, subfolder: str | None, device: str | None,
                 backend: str = "laya"):
        self.name = backend
        if backend == "laya-mlx":
            import laya_mlx as laya
        else:
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
    if req.get("method") == "hello":
        want = req.get("params", {}).get("protocol")
        if want != PROTOCOL:
            return {
                "id": req.get("id"), "ok": False,
                "error": f"protocol {want} not supported (worker speaks {PROTOCOL})",
            }
        return {
            "id": req.get("id"), "ok": True,
            "protocol": PROTOCOL, "worker": "laya-worker",
        }
    params = req.get("params", {})
    state = params.get("state", "")
    questions = params.get("questions", [])
    answers = provider.predict(state, questions)
    return {"id": req.get("id"), "ok": True, "provider": provider.name, "answers": answers}


def build_parser() -> argparse.ArgumentParser:
    ap = argparse.ArgumentParser()
    ap.add_argument("--provider", default="dev", choices=["dev", "laya", "laya-mlx"])
    ap.add_argument("--model", default="convaiinnovations/laya",
                    help="HF model id or local checkpoint dir (default: laya family repo)")
    ap.add_argument("--subfolder", default="multilingual",
                    help="checkpoint subfolder: multilingual (localized UIs) | "
                         "typed-decisions | root for the english checkpoint")
    ap.add_argument("--device", default=None,
                    help="laya: cpu | cuda | mps (default: auto); "
                         "laya-mlx: gpu | metal | cpu (default: Metal GPU)")
    return ap


def main(argv=None) -> int:
    args = build_parser().parse_args(argv)

    provider = DevProvider()
    if args.provider != "dev":
        # Load once, fail fast at startup rather than mid-session.
        try:
            subfolder = None if args.subfolder in ("", "root") else args.subfolder
            provider = LayaProvider(args.model, subfolder, args.device,
                                    backend=args.provider)
        except Exception as e:
            print(json.dumps({
                "id": None, "ok": False,
                "error": f"{args.provider} provider failed to load: {e}",
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
    sys.exit(main())  # pragma: no cover
