#!/usr/bin/env python3
"""Seam tests for the laya worker + training-row helpers.

Stdlib only — no torch, no mlx, no model downloads. Run:

    python3 -m unittest workers/laya/test_worker.py -v
"""
import io
import json
import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(__file__))

import rows  # noqa: E402
import worker  # noqa: E402


def _row(rid, app, n_cands=3, n_routes=4, gold=0):
    opts = [f"candidate {i}: click — el{i}" for i in range(n_cands)]
    opts += [f"route{i}" for i in range(n_routes)]
    return {
        "id": rid,
        "app": app,
        "state": "[GOAL]\nx",
        "options": opts,
        "n_candidates": n_cands,
        "gold_index": gold,
        "gold_route": None,
    }


class TestRows(unittest.TestCase):
    def test_criteria_maps_candidates_and_routes(self):
        crit = rows.criteria_from_options(
            ["candidate 0: click — Save", "candidate 1: type — x", "abstain — no fit"]
        )
        self.assertEqual(crit, {"c0": "click — Save", "c1": "type — x", "r2": "abstain — no fit"})

    def test_load_rows_drops_uncovered_golds(self):
        import tempfile

        rows_in = [_row("a", "app1", gold=0), {**_row("b", "app1"), "gold_index": None}]
        with tempfile.NamedTemporaryFile("w", suffix=".jsonl", delete=False) as f:
            for r in rows_in:
                f.write(json.dumps(r) + "\n")
            path = f.name
        try:
            out = rows.load_rows(path)
        finally:
            os.unlink(path)
        self.assertEqual([r["id"] for r in out], ["a"])

    def test_split_rows_never_splits_an_item(self):
        data = [_row(f"i{i}", "app", gold=0) for i in range(10)]
        # a second row under the same id (e.g. exported twice)
        data.append(_row("i0", "app", gold=1))
        train, test = rows.split_rows(data, holdout=0.3, seed=0)
        train_ids = {r["id"] for r in train}
        test_ids = {r["id"] for r in test}
        self.assertFalse(train_ids & test_ids)
        self.assertEqual(len(train) + len(test), len(data))
        self.assertGreaterEqual(len(test_ids), 1)

    def test_holdout_app_prefix(self):
        data = [_row("a", "com.apple.calculator"), _row("b", "sim/Files"), _row("c", "com.apple.finder")]
        train, held = rows.holdout_app(data, ["com.apple.calculator"])
        self.assertEqual([r["id"] for r in held], ["a"])
        self.assertEqual({r["id"] for r in train}, {"b", "c"})
        train, held = rows.holdout_app(data, ["com.apple.", "sim/"])
        self.assertEqual([r["id"] for r in train], [])
        self.assertEqual(len(held), 3)


class TestWorkerProtocol(unittest.TestCase):
    def test_hello(self):
        resp = worker.handle(
            {"id": 0, "method": "hello", "params": {"protocol": worker.PROTOCOL}}, worker.DevProvider()
        )
        self.assertTrue(resp["ok"])
        self.assertEqual(resp["protocol"], worker.PROTOCOL)

    def test_hello_rejects_other_versions(self):
        resp = worker.handle({"id": 0, "method": "hello", "params": {"protocol": 99}}, worker.DevProvider())
        self.assertFalse(resp["ok"])

    def test_dev_predict_labels_provider(self):
        resp = worker.handle(
            {
                "id": 1,
                "method": "predict",
                "params": {
                    "state": "[GOAL]\nsave the file\n",
                    "questions": [
                        {
                            "id": "pick",
                            "type": "choice",
                            "options": ["click — save file", "click — delete"],
                        }
                    ],
                },
            },
            worker.DevProvider(),
        )
        self.assertTrue(resp["ok"])
        self.assertEqual(resp["provider"], "dev")
        self.assertEqual(resp["answers"][0]["index"], 0)

    def test_parser_accepts_laya_mlx(self):
        args = worker.build_parser().parse_args(["--provider", "laya-mlx", "--device", "gpu"])
        self.assertEqual(args.provider, "laya-mlx")
        self.assertEqual(args.device, "gpu")

    def test_parser_rejects_unknown_providers(self):
        with self.assertRaises(SystemExit):
            worker.build_parser().parse_args(["--provider", "bogus"])

    def test_main_fails_honest_when_backend_missing(self):
        # dev never imports a model SDK; an unimportable provider must
        # exit 2 with ok:false on stdout — never a traceback, never dev.
        stderr, stdout = sys.stderr, sys.stdout
        sys.stdout, sys.stderr = io.StringIO(), io.StringIO()
        try:
            code = worker.main(["--provider", "laya-mlx", "--model", "/nonexistent-dir"])
            out = [json.loads(l) for l in sys.stdout.getvalue().splitlines() if l.strip()]
            err_text = sys.stderr.getvalue()
        finally:
            sys.stdout, sys.stderr = stdout, stderr
        self.assertEqual(code, 2)
        self.assertEqual(len(out), 1)
        self.assertFalse(out[0]["ok"])
        self.assertIn("laya-mlx", out[0]["error"].lower() + err_text.lower())


if __name__ == "__main__":
    unittest.main()
