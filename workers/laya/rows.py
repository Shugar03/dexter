"""Shared training-row helpers for the laya fine-tune trainers.

`dexter eval export` emits one JSON object per frozen eval item:
{id, app, state, options, n_candidates, gold_index|null, gold_route}.
`gold_index` is the absolute option index (candidates first, then the
fixed route options); `null` marks an uncovered/ambiguous gold — not
trainable. `app` is the provenance key `eval matrix` groups by
(`meta.app → meta.url → observation.app`), so leave-one-app-out
splits here match the matrix's per-app rows exactly.

Used by finetune.py (torch) and finetune_mlx.py (MLX); covered by
test_worker.py — stdlib only, no model SDKs imported here.
"""
import json
import random
import re


def criteria_from_options(options):
    """Worker-identical option -> criteria mapping (c{i}/r{i} keys)."""
    crit = {}
    for i, opt in enumerate(options):
        if re.match(r"^candidate \d+:", opt):
            crit[f"c{i}"] = re.sub(r"^candidate \d+:\s*", "", opt)
        else:
            crit[f"r{i}"] = opt
    return crit


def load_rows(path):
    rows = [json.loads(l) for l in open(path) if l.strip()]
    return [r for r in rows if r["gold_index"] is not None]


def split_rows(rows, holdout: float, seed: int):
    """Hold out whole items (by id), stratified-ish across the file."""
    rng = random.Random(seed)
    ids = list(dict.fromkeys(r["id"] for r in rows))
    rng.shuffle(ids)
    n_held = max(1, round(len(ids) * holdout))
    held = set(ids[:n_held])
    train = [r for r in rows if r["id"] not in held]
    test = [r for r in rows if r["id"] in held]
    return train, test


def holdout_app(rows, prefixes):
    """Leave-one-app-out: hold out every row whose `app` provenance key
    starts with any prefix; train on the rest. Mirrors `eval matrix`'s
    per-app rows, so the held-out group IS the matrix row."""
    prefixes = [p for p in prefixes if p]
    held = [r for r in rows if any(r["app"].startswith(p) for p in prefixes)]
    train = [r for r in rows if not any(r["app"].startswith(p) for p in prefixes)]
    return train, held


# The instruction text must match the choice question the engine emits
# (crates/laya build_question) — it is part of the input distribution.
PROMPT = (
    "You are choosing the next action for a computer-use agent. "
    "Pick the UI element whose action best advances the goal in [GOAL]. "
    "If no element fits, pick a route: 'wait' for busy/loading states, "
    "'re-observe' when the view may be stale, 'abstain' when nothing "
    "applies, or 'escalate' for genuinely hard steps. Priors in the "
    "option text are heuristic hints, not truth."
)
