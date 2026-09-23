#!/usr/bin/env python3
"""Fine-tune a Laya checkpoint on Dexter's frozen eval items.

The eval items already freeze the exact decision distribution the model
sees at inference: `dexter eval export` renders (state, options) through
the same code path as LayaEngine.decide. This script:

  1. group-splits rows by item id (permutations stay in their split)
  2. augments with option-order permutations — teaches order invariance
  3. freezes the encoder and trains the decision head on cached hidden
     states (exact — encoder output is constant per input — and ~50x
     faster than full forward passes on CPU)
  4. trains with laya's own proper scoring-rule loss, plus a BCE term on
     the act head (act vs don't-act)
  5. refits the softmax temperature on the training split so reported
     confidence stays meaningful
  6. writes a loadable checkpoint dir: model.safetensors +
     rl_agent_config.json + tokenizer/ + encoder/

Usage:
  dexter eval export datasets/browser/items.jsonl datasets/macos/items.jsonl -o rows.jsonl
  python3 workers/laya/finetune.py rows.jsonl --out models/dexter-laya \
      --base convaiinnovations/laya --subfolder root --epochs 60 --perms 8
"""
import argparse
import json
import os
import random
import re
import shutil
import sys

import numpy as np
import torch
import torch.nn.functional as F

sys.path.insert(0, os.path.dirname(__file__))

# Route option text -> variant, matching ROUTE_OPTIONS order in the engine.
ROUTE_VARIANTS = ["wait", "reobserve", "abstain", "escalate_llm"]


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


def build_examples(rows, tok, cfg, perms: int, seed: int, prompt: str):
    """Rows -> tokenized items. Each row yields `perms` permuted copies;
    the gold index is remapped to its position in the permutation."""
    from laya.common import QTYPES, build_sequence

    rng = random.Random(seed)
    max_len = cfg.get("max_len", 512)
    head_max = cfg.get("head_max_len", 192)
    out = []
    for row in rows:
        opts = row["options"]
        k = len(opts)
        crit = criteria_from_options(opts)
        q = {"t": "choice", "ins": prompt, "crit": crit}
        orders = [list(range(k))] + [rng.sample(range(k), k) for _ in range(perms - 1)]
        for order in orders:
            # build_sequence applies option_order over rendered options;
            # markers then follow the permuted order.
            ids, markers = build_sequence(
                tok, row["state"], q, max_len, head_max, option_order=order
            )
            gold_pos = order.index(row["gold_index"])
            is_act = 1.0 if row["gold_index"] < row["n_candidates"] else 0.0
            out.append({
                "ids": ids,
                "markers": markers,
                "qtype": QTYPES["choice"],
                "label": gold_pos,
                "act": is_act,
                "item": row["id"],
            })
    return out


def collate(examples, pad_id):
    from laya.common import collate_items

    items = [{
        "ids": e["ids"], "markers": e["markers"], "qtype": e["qtype"],
        "label": e["label"],
    } for e in examples]
    return collate_items([items], pad_id), examples


def head_forward(model, h, attention_mask, marker_pos, marker_mask, qtype):
    """Post-encoder part of DecisionModel.forward — identical math,
    operates on cached encoder output."""
    h = h + model.type_emb(qtype)[:, None, :]
    pad = ~attention_mask.bool()
    if model.head is not None:
        for layer in model.head.layers:
            h = layer(h, src_key_padding_mask=pad)
    idx = marker_pos.clamp(min=0)[:, :, None].expand(-1, -1, h.size(-1))
    m = torch.gather(h, 1, idx)
    logits = model.scorer(m).squeeze(-1).float()
    logits = logits.masked_fill(~marker_mask, -1e4)
    p = torch.softmax(logits.detach(), -1)
    k = marker_mask.sum(-1).clamp(min=2).float()
    ent = -(p * torch.log(p.clamp_min(1e-9))).sum(-1) / torch.log(k)
    if p.size(-1) >= 2:
        top2 = p.topk(2, -1).values
    else:
        top1 = p.topk(1, -1).values
        top2 = torch.cat([top1, torch.zeros_like(top1)], dim=-1)
    feats = torch.stack([top2[:, 0], top2[:, 0] - top2[:, 1], ent, k / 255.0], -1)
    pooled = h[:, 0].float()
    act_logits = model.act_head(torch.cat([pooled, feats], -1))
    return logits, act_logits


def encoder_hidden(model, batch, device):
    with torch.no_grad():
        return model.encoder(
            input_ids=batch["input_ids"].to(device),
            attention_mask=batch["attention_mask"].to(device),
        ).last_hidden_state


def evaluate(model, examples, pad_id, device):
    """Argmax accuracy over a set of examples (head math on fresh
    encoder output — mirrors inference)."""
    from laya.common import confidence_from_probs

    correct, confs, rights = 0, [], []
    batch, exs = collate(examples, pad_id)
    with torch.no_grad():
        h = encoder_hidden(model, batch, device)
        logits, _ = head_forward(
            model, h,
            batch["attention_mask"].to(device),
            batch["marker_pos"].to(device),
            batch["marker_mask"].to(device),
            batch["qtype"].to(device),
        )
    probs = torch.softmax(logits, -1).cpu().numpy()
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


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("rows", help="JSONL from `dexter eval export`")
    ap.add_argument("--out", required=True, help="output checkpoint dir")
    ap.add_argument("--base", default="convaiinnovations/laya")
    ap.add_argument("--subfolder", default="root",
                    help="'root' = english base, 'multilingual', ...")
    ap.add_argument("--epochs", type=int, default=60)
    ap.add_argument("--perms", type=int, default=8,
                    help="option-order permutations per row (1 = none)")
    ap.add_argument("--holdout", type=float, default=0.2)
    ap.add_argument("--lr", type=float, default=3e-4)
    ap.add_argument("--seed", type=int, default=0)
    ap.add_argument("--device", default=None, help="cpu | mps | cuda (default: auto)")
    args = ap.parse_args()

    import laya
    from laya.common import build_model, proper_reward

    rows = load_rows(args.rows)
    print(f"{len(rows)} usable rows", flush=True)
    train_rows, test_rows = split_rows(rows, args.holdout, args.seed)
    print(f"train {len(train_rows)} | holdout {len(test_rows)}", flush=True)

    # Load base checkpoint the same way Agent does, but keep the model
    # in train mode for the head.
    sub = None if args.subfolder == "root" else args.subfolder
    agent = laya.Agent(args.base, subfolder=sub, device=args.device)
    model = agent.model
    cfg = agent.cfg
    device = agent.device
    tok = agent.tok

    PROMPT = ("You are choosing the next action for a computer-use agent. "
              "Pick the UI element whose action best advances the goal in [GOAL]. "
              "If no element fits, pick a route: 'wait' for busy/loading states, "
              "'re-observe' when the view may be stale, 'abstain' when nothing "
              "applies, or 'escalate' for genuinely hard steps. Priors in the "
              "option text are heuristic hints, not truth.")

    train_ex = build_examples(train_rows, tok, cfg, args.perms, args.seed, PROMPT)
    test_ex = build_examples(test_rows, tok, cfg, 1, args.seed, PROMPT)
    # Also measure in-sample (unpermuted) for the overfitting report.
    insample_ex = build_examples(train_rows, tok, cfg, 1, args.seed, PROMPT)
    print(f"{len(train_ex)} train examples ({args.perms} perms), "
          f"{len(test_ex)} held-out", flush=True)

    # Freeze encoder — we train the decision head on cached hidden
    # states (exact: encoder output is constant per input).
    for p in model.encoder.parameters():
        p.requires_grad_(False)
    model.train()

    # Cache encoder output once per unique example — in eval mode so no
    # dropout noise enters the frozen representation. Chunked: one giant
    # batch of hidden states doesn't fit in unified memory.
    print("caching encoder states...", flush=True)
    model.eval()
    chunk = 32
    cached = []  # per-chunk (h, batch)
    for lo in range(0, len(train_ex), chunk):
        b, _ = collate(train_ex[lo:lo + chunk], tok.pad_token_id)
        h = encoder_hidden(model, b, device).detach()
        cached.append((h, b))
        print(f"  cached {min(lo + chunk, len(train_ex))}/{len(train_ex)}",
              flush=True)
    model.train()  # head dropout on; encoder output is already cached

    opt = torch.optim.AdamW(
        [p for n, p in model.named_parameters() if p.requires_grad and "encoder." not in n],
        lr=args.lr, weight_decay=0.01,
    )

    rng = random.Random(args.seed)
    for epoch in range(args.epochs):
        order = list(range(len(cached)))
        rng.shuffle(order)
        tot = 0.0
        for ci in order:
            h, b = cached[ci]
            att = b["attention_mask"].to(device)
            mpos = b["marker_pos"].to(device)
            mmask = b["marker_mask"].to(device)
            qt = b["qtype"].to(device)
            lab = b["label"].to(device)
            opt.zero_grad()
            logits, act_logits = head_forward(model, h, att, mpos, mmask, qt)
            k = logits.size(1)
            target = torch.zeros(logits.size(0), k, device=device)
            target.scatter_(1, lab[:, None], 1.0)
            q = torch.softmax(logits, -1)
            r = proper_reward(q, target, qt, mmask)
            acts = torch.tensor(
                [[e["act"], 1.0 - e["act"]] for e in
                 train_ex[ci * chunk:ci * chunk + h.size(0)]],
                device=device)
            loss = -r.mean() + 0.3 * F.binary_cross_entropy_with_logits(
                act_logits, acts)
            loss.backward()
            opt.step()
            tot += loss.item()
        if (epoch + 1) % 10 == 0 or epoch == 0:
            print(f"epoch {epoch+1}: loss {tot / len(cached):.4f}", flush=True)

    model.eval()
    tr_acc, _, _ = evaluate(model, insample_ex, tok.pad_token_id, device)
    te_acc, confs, rights = evaluate(model, test_ex, tok.pad_token_id, device)
    from laya.common import ece_score
    print(f"\nfinal: train acc {tr_acc:.2f} | holdout acc {te_acc:.2f} | "
          f"holdout ECE {ece_score(confs, rights):.3f}", flush=True)

    # --- save a loadable checkpoint dir ------------------------------
    base_dir = args.base
    if not os.path.isdir(base_dir):
        from huggingface_hub import snapshot_download
        prefix = f"{sub}/" if sub else ""
        base_dir = snapshot_download(args.base, allow_patterns=[
            prefix + n for n in ("rl_agent_config.json", "model.safetensors",
                                 "tokenizer/*", "encoder/*")])
    if sub:
        base_dir = os.path.join(base_dir, sub)
    os.makedirs(args.out, exist_ok=True)
    for name in ("tokenizer", "encoder", "rl_agent_config.json"):
        src = os.path.join(base_dir, name)
        dst = os.path.join(args.out, name)
        if os.path.isdir(src):
            if os.path.exists(dst):
                shutil.rmtree(dst)
            shutil.copytree(src, dst)
        elif os.path.exists(src):
            shutil.copy(src, dst)
    from safetensors.torch import save_file
    save_file(model.state_dict(), os.path.join(args.out, "model.safetensors"))
    # Temperatures from the base checkpoint no longer describe these
    # logits — ship 1.0s (honest, uncalibrated-by-default) rather than a
    # fitted-on-31-rows table that pretends precision it doesn't have.
    cfg_out = dict(cfg)
    cfg_out["temperature"] = [1.0, 1.0, 1.0]
    cfg_out["temperature_by_options"] = {}
    cfg_out.setdefault("training", {})["dexter_finetune"] = {
        "rows": len(rows), "perms": args.perms, "epochs": args.epochs,
        "frozen_encoder": True,
        "train_acc": round(tr_acc, 4), "holdout_acc": round(te_acc, 4),
    }
    with open(os.path.join(args.out, "rl_agent_config.json"), "w") as f:
        json.dump(cfg_out, f, indent=2)
    print(f"checkpoint written to {args.out}", flush=True)


if __name__ == "__main__":
    main()
