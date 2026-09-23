"""SDK contract tests: handshake, call round-trip, error surfacing,
notification skipping, timeout — against a fake MCP server. A real
`dexter mcp` integration test runs only when DEXTER_BIN is set.
"""
import os
import shutil
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from dexter import Dexter, DexterError, DexterTimeout  # noqa: E402

FAKE = str(Path(__file__).parent / "fake_mcp_server.py")


def make(**kw) -> Dexter:
    kw.setdefault("timeout", 5.0)
    return Dexter(command=sys.executable, args=(FAKE,), **kw)


class TestAgainstFakeServer(unittest.TestCase):
    def test_handshake_and_observe(self):
        with make() as d:
            obs = d.observe()
            self.assertEqual(obs["observation"], 1)
            self.assertEqual(obs["digest"], "sim")

    def test_status_parses_json_content(self):
        with make() as d:
            s = d.status()
            self.assertEqual(s["driver"]["name"], "fake")
            self.assertEqual(s["engine"]["health"]["status"], "ready")

    def test_error_result_raises(self):
        with make() as d:
            with self.assertRaises(DexterError) as ctx:
                d._call("fails")
            self.assertIn("boom", str(ctx.exception))

    def test_unknown_tool_raises_rpc_error(self):
        with make() as d:
            with self.assertRaises(DexterError):
                d._call("nope")

    def test_timeout_surfaces(self):
        with make(timeout=0.5) as d:
            with self.assertRaises(DexterTimeout):
                d._call("never_respond")

    def test_task_args_passthrough(self):
        with make() as d:
            # cancel on the fake returns {"cancelled": false}
            self.assertEqual(d.cancel()["cancelled"], False)


class TestRealServer(unittest.TestCase):
    """Smoke against the real binary — opt-in via DEXTER_BIN."""

    def test_real_handshake_and_status(self):
        binary = os.environ.get("DEXTER_BIN") or shutil.which("dexter")
        if not binary:
            self.skipTest("no dexter binary (set DEXTER_BIN)")
        with Dexter(command=binary, timeout=15.0) as d:
            s = d.status()
            self.assertIn(s["driver"]["name"], ("macos", "sim", "browser"))
            self.assertIn("engine", s)
            obs = d.observe(max_elements=50)
            self.assertIn("observation", obs)


if __name__ == "__main__":
    unittest.main()
