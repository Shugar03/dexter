#!/usr/bin/env python3
"""Fine-tune a laya-mlx checkpoint on Dexter's frozen eval items.

MLX-native port of finetune.py (Apple silicon — `pip install laya-mlx`).
Same recipe, same honest-split protocol:

  1. group-splits rows by item id (--holdout) or by `eval export`
     provenance prefix (--holdout-app, leave-one-app-out — the held-out
     group's `eval matrix` row is then the transfer measurement)
  2. augments with option-order permutations — teaches order invariance
     (the published MLX ports are `option_layout: sequential`, so the
     head still sees option position)
  3. freezes the encoder and trains the decision head on cached hidden
     states (exact — encoder output is constant per input)
  4. trains with laya's proper scoring-rule loss, plus a BCE term on
     the act head (act vs don't-act)
  5. writes a loadable laya-mlx checkpoint dir: model.safetensors +
     rl_agent_config.json + tokenizer/ + encoder/ + mlx_config.json

Usage:
  dexter eval export datasets/browser/items.jsonl datasets/macos/items.jsonl -o rows.jsonl
  python3 workers/laya/finetune_mlx.py rows.jsonl --out /tmp/dexter-laya-mlx \
      --base aac6fef/laya-mlx --epochs 60 --perms 8
  # honest transfer split (train minus one app, eval on its matrix row):
  python3 workers/laya/finetune_mlx.py rows.jsonl --out /tmp/ft-minus-sim \
      --base aac6fef/laya-mlx --holdout-app sim/
"""
import argparse
import json
import os
import random
import shutil
import sys

import numpy as np

sys.path.insert(0, os.path.dirname(__file__))
from rows import PROMPT, criteria_from_options, holdout_app, load_rows, split_rows  # noqa: E402


def build_examples(rows, tok, cfg, perms: int, seed: int, prompt: str):
    """Rows -> tokenized items. Each row yields `perms` permuted copies;
    the gold index is remapped to its position in the permutation.
    `layout` is kept only for parallel-layout checkpoints."""
    from laya_mlx.common import QTYPES, build_sequence, uses_parallel_layout

    rng = random.Random(seed)
    max_len = cfg.get("max_len", 512)
    head_max = cfg.get("head_max_len", 192)
    parallel = uses_parallel_layout(cfg)
    out = []
    for row in rows:
        opts = row["options"]
        k = len(opts)
        crit = criteria_from_options(opts)
        q = {"t": "choice", "ins": prompt, "crit": crit}
        orders = [list(range(k))] + [rng.sample(range(k), k) for _ in range(perms - 1)]
        for order in orders:
            seq = build_sequence(
                tok, row["state"], q, max_len, head_max,
                option_order=order, return_layout=parallel,
            )
            ids, markers = seq[0], seq[1]
            gold_pos = order.index(row["gold_index"])
            is_act = 1.0 if row["gold_index"] < row["n_candidates"] else 0.0
            ex = {
                "ids": ids,
                "markers": markers,
                "qtype": QTYPES["choice"],
                "label": gold_pos,
                "act": is_act,
                "item": row["id"],
            }
            if parallel:
                ex["layout"] = seq[2]
            out.append(ex)
    return out


def collate(examples, pad_id):
    from laya_mlx.agent import collate_items

    items = [{
        "ids": e["ids"], "markers": e["markers"], "qtype": e["qtype"],
        "label": e["label"],
        **({"layout": e["layout"]} if "layout" in e else {}),
    } for e in examples]
    return collate_items(items, pad_id), examples


def encoder_hidden(model, batch, mx):
    """Frozen-encoder forward — the part of DecisionModel.__call__
    before the type embedding."""
    kwargs = {}
    if "position_ids" in batch:
        kwargs["position_ids"] = mx.array(batch["position_ids"])
        kwargs["option_ids"] = mx.array(batch["option_ids"])
    return model.encoder(
        mx.array(batch["input_ids"]), mx.array(batch["attention_mask"]), **kwargs
    )


def head_forward(model, h, attention_mask, marker_pos, marker_mask, qtype):
    """Post-encoder part of laya_mlx DecisionModel.__call__ — identical
    math, operates on cached encoder output."""
    import mlx.core as mx

    h = h + model.type_emb(qtype)[:, None, :]
    h = model.head(h, attention_mask[:, None, None, :].astype(mx.bool_))
    markers = h[mx.arange(h.shape[0])[:, None], mx.maximum(marker_pos, 0)]
    if model.parallel:
        # Same FP32 accumulation path the runtime uses (see model.py).
        norm, hidden, activation, output = model.scorer.layers
        features = activation(hidden(norm(markers))).astype(mx.float32)
        logits = (
            features @ output.weight.astype(mx.float32).T + output.bias.astype(mx.float32)
        ).squeeze(-1)
    else:
        logits = model.scorer(markers).squeeze(-1).astype(mx.float32)
    logits = mx.where(marker_mask, logits, -1e4)
    p = mx.softmax(logits, axis=-1)
    k = mx.maximum(marker_mask.sum(axis=-1), 2).astype(mx.float32)
    entropy = -(p * mx.log(mx.maximum(p, 1e-9))).sum(axis=-1) / mx.log(k)
    top = mx.sort(p, axis=-1)[:, -2:]
    features = mx.stack([top[:, 1], top[:, 1] - top[:, 0], entropy, k / 255.0], axis=-1)
    pooled = mx.concatenate([h[:, 0].astype(mx.float32), features], axis=-1)
    if model.parallel and mx.default_device() == mx.cpu:
        first, activation, last = model.act_head.layers
        hidden = activation(
            pooled @ first.weight.astype(mx.float32).T + first.bias.astype(mx.float32)
        )
        action = hidden @ last.weight.astype(mx.float32).T + last.bias.astype(mx.float32)
    else:
        action = model.act_head(pooled.astype(model.act_head.layers[0].weight.dtype))
    return logits, action.astype(mx.float32)


def proper_reward(q, target, qtype, mask, w_sph=0.5, w_rps=1.0, log_floor=-9.21):
    """Strictly proper scoring-rule reward — MLX port of upstream
    laya.common.proper_reward: log score + spherical score + ranked
    probability score (score questions only)."""
    import mlx.core as mx
    from laya_mlx.common import QTYPES

    q = q * mask
    logq = mx.maximum(mx.log(mx.maximum(q, 1e-12)), log_floor)
    log_score = (target * logq).sum(-1)
    norm = mx.sqrt((q * q).sum(-1))
    sph = (target * q).sum(-1) / mx.maximum(norm, 1e-9)
    r = log_score + w_sph * sph
    is_score = (qtype == QTYPES["score"]).astype(mx.float32)
    k = mx.maximum(mask.sum(-1), 2).astype(mx.float32)
    cdf_q = mx.cumsum(q, axis=-1)
    cdf_t = mx.cumsum(target, axis=-1)
    rps = (((cdf_q - cdf_t) ** 2) * mask).sum(-1) / (k - 1)
    return r - w_rps * rps * is_score


def evaluate(model, examples, pad_id):
    """Argmax accuracy over a set of examples — full forward through
    encoder + head (mirrors inference)."""
    import mlx.core as mx
    from laya_mlx.common import confidence_from_probs

    correct, confs, rights = 0, [], []
    batch, exs = collate(examples, pad_id)
    tensors = {k: mx.array(v) for k, v in batch.items()}
    logits, _ = model(**tensors)
    probs = mx.softmax(logits, axis=-1)
    probs = np.asarray(probs)
    for i, e in enumerate(exs):
        k = len(e["markers"])
        p = probs[i, :k]
        pred = int(p.argmax())
        ok = pred == e["label"]
        correct += ok
        confs.append(confidence_from_probs(p, k))
        rights.append(float(ok))
    acc = correct / max(1, len(exs))
    return acc, np.array(confs), np.array(rights)


def ece_score(conf, correct, bins=15):
    """Expected Calibration Error across confidence bins (numpy port of
    upstream laya.common.ece_score)."""
    conf, correct = np.asarray(conf), np.asarray(correct)
    if len(conf) == 0:
        return float("nan")
    edges = np.linspace(0, 1, bins + 1)
    e = 0.0
    for i, (lo, hi) in enumerate(zip(edges[:-1], edges[1:])):
        sel = (conf >= lo if i == 0 else conf > lo) & (conf <= hi)
        if sel.any():
            e += sel.mean() * abs(conf[sel].mean() - correct[sel].mean())
    return float(e)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("rows", help="JSONL from `dexter eval export`")
    ap.add_argument("--out", required=True, help="output checkpoint dir")
    ap.add_argument("--base", default="aac6fef/laya-mlx",
                    help="HF model id or local dir — MLX checkpoint to start from")
    ap.add_argument("--subfolder", default=None,
                    help="checkpoint subfolder inside --base (default: none)")
    ap.add_argument("--epochs", type=int, default=60)
    ap.add_argument("--perms", type=int, default=8,
                    help="option-order permutations per row (1 = none)")
    ap.add_argument("--holdout", type=float, default=0.2,
                    help="random item holdout fraction (default 0.2; 0 = none)")
    ap.add_argument("--holdout-app", action="append", default=[],
                    metavar="PREFIX",
                    help="hold out every row whose `app` starts with PREFIX "
                         "(repeatable; leave-one-app-out, overrides --holdout)")
    ap.add_argument("--lr", type=float, default=3e-4)
    ap.add_argument("--seed", type=int, default=0)
    ap.add_argument("--chunk", type=int, default=32, help="examples per cached batch")
    ap.add_argument("--device", default=None,
                    help="gpu | metal | cpu (default: Metal GPU)")
    ap.add_argument("--dtype", default="float32", choices=["float32", "float16"],
                    help="training precision (default float32 — stable AdamW)")
    ap.add_argument("--save-dtype", default="float16", choices=["float16", "float32"],
                    help="checkpoint dtype (default float16 — the published format)")
    args = ap.parse_args()

    import mlx.core as mx
    import mlx.nn as nn
    import mlx.optimizers as optim
    from mlx.utils import tree_flatten
    import laya_mlx

    rows = load_rows(args.rows)
    print(f"{len(rows)} usable rows", flush=True)
    if args.holdout_app:
        train_rows, test_rows = holdout_app(rows, args.holdout_app)
        held_desc = f"app {args.holdout_app}"
    elif args.holdout <= 0:
        train_rows, test_rows = rows, []
        held_desc = "none (in-sample fit)"
    else:
        train_rows, test_rows = split_rows(rows, args.holdout, args.seed)
        held_desc = f"random {args.holdout:.0%} of items"
    print(f"train {len(train_rows)} | holdout {len(test_rows)} ({held_desc})",
          flush=True)

    agent = laya_mlx.load(args.base, subfolder=args.subfolder,
                          device=args.device, dtype=args.dtype)
    model = agent.model
    cfg = agent.cfg
    tok = agent.tok

    train_ex = build_examples(train_rows, tok, cfg, args.perms, args.seed, PROMPT)
    test_ex = build_examples(test_rows, tok, cfg, 1, args.seed, PROMPT)
    insample_ex = build_examples(train_rows, tok, cfg, 1, args.seed, PROMPT)
    print(f"{len(train_ex)} train examples ({args.perms} perms), "
          f"{len(test_ex)} held-out", flush=True)

    # Freeze everything, then re-enable the decision head — identical
    # trainable surface to finetune.py (head + type_emb + scorer +
    # act_head), and optimizer updates can't touch encoder weights.
    model.freeze()
    for mod in (model.head, model.type_emb, model.scorer, model.act_head):
        mod.unfreeze()
    n_train = sum(v.size for _, v in tree_flatten(model.trainable_parameters()))
    print(f"trainable params: {n_train/1e6:.1f}M (encoder frozen)", flush=True)

    # Cache encoder output once per unique example. Chunked: one giant
    # batch of hidden states doesn't fit in unified memory.
    print("caching encoder states...", flush=True)
    cached = []  # per-chunk (h, batch)
    for lo in range(0, len(train_ex), args.chunk):
        b, _ = collate(train_ex[lo:lo + args.chunk], tok.pad_token_id)
        h = encoder_hidden(model, b, mx)
        mx.eval(h)
        cached.append((h, b))
        print(f"  cached {min(lo + args.chunk, len(train_ex))}/{len(train_ex)}",
              flush=True)

    opt = optim.AdamW(learning_rate=args.lr, weight_decay=0.01)

    def loss_fn(model, h, att, mpos, mmask, qt, lab, acts):
        logits, act_logits = head_forward(model, h, att, mpos, mmask, qt)
        k = logits.shape[1]
        target = mx.zeros((logits.shape[0], k))
        target[mx.arange(logits.shape[0]), lab] = 1.0
        q = mx.softmax(logits, axis=-1)
        r = proper_reward(q, target, qt, mmask.astype(mx.float32))
        bce = nn.losses.binary_cross_entropy(act_logits, acts, with_logits=True)
        return -r.mean() + 0.3 * bce

    loss_and_grad = nn.value_and_grad(model, loss_fn)

    rng = random.Random(args.seed)
    for epoch in range(args.epochs):
        order = list(range(len(cached)))
        rng.shuffle(order)
        tot = 0.0
        for ci in order:
            h, b = cached[ci]
            att = mx.array(b["attention_mask"])
            mpos = mx.array(b["marker_pos"])
            mmask = mx.array(b["marker_mask"])
            qt = mx.array(b["qtype"])
            exs = train_ex[ci * args.chunk:ci * args.chunk + h.shape[0]]
            lab = mx.array([e["label"] for e in exs])
            acts = mx.array([[e["act"], 1.0 - e["act"]] for e in exs])
            loss, grads = loss_and_grad(model, h, att, mpos, mmask, qt, lab, acts)
            opt.update(model, grads)
            mx.eval(model.parameters(), opt.state)
            tot += loss.item()
        if (epoch + 1) % 10 == 0 or epoch == 0:
            print(f"epoch {epoch+1}: loss {tot / len(cached):.4f}", flush=True)

    tr_acc, _, _ = evaluate(model, insample_ex, tok.pad_token_id)
    if test_ex:
        te_acc, confs, rights = evaluate(model, test_ex, tok.pad_token_id)
        print(f"\nfinal: train acc {tr_acc:.2f} | holdout acc {te_acc:.2f} | "
              f"holdout ECE {ece_score(confs, rights):.3f}", flush=True)
    else:
        te_acc = float("nan")
        print(f"\nfinal: train acc {tr_acc:.2f} | no holdout (in-sample fit)",
              flush=True)

    # --- save a loadable checkpoint dir ------------------------------
    base_dir = agent.model_dir
    os.makedirs(args.out, exist_ok=True)
    for name in ("tokenizer", "encoder", "rl_agent_config.json", "mlx_config.json"):
        src = os.path.join(base_dir, name)
        dst = os.path.join(args.out, name)
        if os.path.isdir(src):
            if os.path.exists(dst):
                shutil.rmtree(dst)
            shutil.copytree(src, dst)
        elif os.path.exists(src):
            shutil.copy(src, dst)
    save_dtype = mx.float16 if args.save_dtype == "float16" else mx.float32
    flat = {k: v.astype(save_dtype) for k, v in tree_flatten(model.parameters())}
    mx.save_safetensors(os.path.join(args.out, "model.safetensors"), flat)
    # Temperatures from the base checkpoint no longer describe these
    # logits — ship 1.0s (honest, uncalibrated-by-default) rather than a
    # fitted-on-N-rows table that pretends precision it doesn't have.
    cfg_out = dict(cfg)
    cfg_out["temperature"] = [1.0, 1.0, 1.0]
    cfg_out["temperature_by_options"] = {}
    cfg_out.setdefault("training", {})["dexter_finetune_mlx"] = {
        "base": args.base,
        "rows": len(rows), "perms": args.perms, "epochs": args.epochs,
        "frozen_encoder": True,
        "holdout": held_desc,
        "train_acc": round(tr_acc, 4),
        "holdout_acc": round(te_acc, 4) if test_ex else None,
    }
    with open(os.path.join(args.out, "rl_agent_config.json"), "w") as f:
        json.dump(cfg_out, f, indent=2)
    mc_path = os.path.join(args.out, "mlx_config.json")
    if os.path.exists(mc_path):
        mc = json.load(open(mc_path))
        mc["dtype"] = args.save_dtype
        mc["fine_tuned_from"] = args.base
        with open(mc_path, "w") as f:
            json.dump(mc, f, indent=2)
    print(f"checkpoint written to {args.out} (dtype {args.save_dtype})", flush=True)


if __name__ == "__main__":
    main()
