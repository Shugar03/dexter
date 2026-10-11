# SDD: laya-mlx — MLX provider + fine-tune seam

## Contract

`workers/laya/worker.py --provider laya-mlx` serves the same NDJSON
predict protocol (protocol 1, `hello` version check) backed by the
`laya-mlx` package (MLX, Apple silicon) instead of the PyTorch `laya`
SDK. Question/answer wire shapes are identical; the only difference is
the `provider` label in responses (`"laya-mlx"`), kept distinct so
journal/audit lines name the runtime that answered.

- `pip install laya-mlx` on Apple silicon; checkpoint ids like
  `aac6fef/laya-mlx` (English root port) or a local checkpoint dir.
- `--model`, `--subfolder`, `--device` keep their meanings
  (`--device gpu|metal|cpu`; default = Metal GPU).
- Missing SDK → the worker prints `ok:false` and exits 2 at startup,
  same fail-honest contract as `--provider laya`. No silent fallback
  to `dev`, ever.

## MLX fine-tune (`workers/laya/finetune_mlx.py`)

Same recipe as `finetune.py`, implemented natively on MLX:

1. group-split rows by item id (`--holdout`) or by `eval export`
   provenance prefix (`--holdout-app` — leave-one-app-out, the same
   key `eval matrix` reports),
2. augment with option-order permutations (the published ports are
   `option_layout: sequential` — position still matters),
3. freeze the encoder, cache hidden states once per example, train
   the decision head (`head`, `type_emb`, `scorer`, `act_head`) with
   the proper scoring-rule loss + BCE on the act head,
4. write a loadable laya-mlx checkpoint dir:
   `model.safetensors` (MLX naming, fp16) + `rl_agent_config.json` +
   `encoder/` + `tokenizer/` + `mlx_config.json`.

`rows.py` holds the row parsing/splitting helpers both trainers share
— one source of truth for `c{i}`/`r{i}` criteria keys and split
semantics.

## Evaluation protocol — honest splits

Numbers are only ever reported against data the checkpoint did NOT
train on:

- `--holdout-app <prefix>` retrains minus-one-group; the held-out
  app's row in `eval matrix` output is the transfer measurement.
- `--holdout <f>` holds out whole items at random; the held-out
  fraction is the in-domain number.
- In-sample fits (trained and scored on the same rows) are labelled
  as such and never used to justify keeping a checkpoint.

A fine-tuned checkpoint is a local artifact — the repo does not
commit model binaries. "Keeping" it means the docs recommend it; if
held-out numbers do not beat the base checkpoint, the verdict and the
tooling ship without a recommended model.
