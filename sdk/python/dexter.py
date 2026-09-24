"""Dexter — Python SDK over the `dexter mcp` stdio server.

Dexter is a computer-use runtime, not an agent: your agent decides,
Dexter observes, gates through policy, acts semantically and verifies.
This SDK is a thin typed client over MCP stdio (newline-delimited
JSON-RPC 2.0) — no dependencies beyond the stdlib.

    from dexter import Dexter

    with Dexter() as d:
        obs = d.observe(app="TextEdit")
        cands = d.candidates("save the document")
        d.act(cands[0]["action"])

Trust is operator-side: pass flags at spawn, never per call —
`Dexter(args=("mcp", "--approve-all"))`. Agents cannot self-approve.
"""

from __future__ import annotations

import json
import queue
import subprocess
import threading
from dataclasses import dataclass, field
from typing import Any, Optional

PROTOCOL_VERSION = "2025-06-18"


class DexterError(RuntimeError):
    """The server returned an isError tool result or a JSON-RPC error."""


class DexterTimeout(TimeoutError):
    """A call exceeded its wall-clock budget — the server may be hung."""


@dataclass
class Dexter:
    """Client for one `dexter mcp` child process.

    `command` is the dexter binary; `args` go verbatim after it (server
    flags like `--driver browser`, `--coords`, `--engine laya` belong
    here — they are operator trust settings, not per-call params).
    `timeout` bounds every tool call.
    """

    command: str = "dexter"
    args: tuple = ("mcp",)
    timeout: float = 30.0
    env: Optional[dict] = None

    _proc: subprocess.Popen = field(init=False, default=None)
    _reader: threading.Thread = field(init=False, default=None)
    _responses: "queue.Queue[dict]" = field(init=False, default=None)
    _next_id: int = field(init=False, default=0)

    def __post_init__(self) -> None:
        self._responses = queue.Queue()
        self._proc = subprocess.Popen(
            [self.command, *self.args],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=None,  # server logs go to the parent's stderr
            env=self.env,
            text=True,
            bufsize=1,
        )
        self._reader = threading.Thread(target=self._read_loop, daemon=True)
        self._reader.start()
        self._handshake()

    # -- lifecycle -----------------------------------------------------

    def _handshake(self) -> None:
        self._request(
            "initialize",
            {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "dexter-python-sdk", "version": "0.1.0"},
            },
        )
        self._notify("notifications/initialized", {})

    def close(self) -> None:
        if self._proc.poll() is None:
            self._proc.terminate()
            try:
                self._proc.wait(timeout=3)
            except subprocess.TimeoutExpired:
                self._proc.kill()

    def __enter__(self) -> "Dexter":
        return self

    def __exit__(self, *_: Any) -> None:
        self.close()

    # -- wire ----------------------------------------------------------

    def _read_loop(self) -> None:
        """stdout lines → response queue. Notifications (no `id`, has
        `method`) are skipped — MCP servers may interleave them."""
        assert self._proc.stdout is not None
        for line in self._proc.stdout:
            line = line.strip()
            if not line:
                continue
            try:
                msg = json.loads(line)
            except json.JSONDecodeError:
                continue
            if "id" in msg:
                self._responses.put(msg)

    def _send(self, msg: dict) -> None:
        assert self._proc.stdin is not None
        self._proc.stdin.write(json.dumps(msg) + "\n")
        self._proc.stdin.flush()

    def _request(self, method: str, params: dict) -> dict:
        self._next_id += 1
        rid = self._next_id
        self._send({"jsonrpc": "2.0", "id": rid, "method": method, "params": params})
        while True:
            try:
                msg = self._responses.get(timeout=self.timeout)
            except queue.Empty:
                raise DexterTimeout(f"{method} did not answer in {self.timeout}s")
            if msg.get("id") != rid:
                continue  # a stale/out-of-order response — keep waiting
            if "error" in msg:
                e = msg["error"]
                raise DexterError(f"{method}: {e.get('code')} {e.get('message')}")
            return msg.get("result", {})

    def _notify(self, method: str, params: dict) -> None:
        self._send({"jsonrpc": "2.0", "method": method, "params": params})

    def _call(self, tool: str, arguments: Optional[dict] = None) -> Any:
        result = self._request(
            "tools/call", {"name": tool, "arguments": arguments or {}}
        )
        if result.get("isError"):
            text = _content_text(result)
            raise DexterError(f"{tool}: {text}")
        text = _content_text(result)
        try:
            return json.loads(text)
        except json.JSONDecodeError:
            return text

    # -- tools ----------------------------------------------------------

    def observe(
        self,
        app: Optional[str] = None,
        window: Optional[int] = None,
        max_elements: Optional[int] = None,
        vision: Optional[bool] = None,
        include_menu: Optional[bool] = None,
    ) -> dict:
        """Snapshot the world: windows[], elements[], digest.

        `vision=True` appends inert OCR elements (evidence only — no
        live handle). `include_menu=False` skips the menubar walk when
        only window controls are needed.
        """
        args = {k: v for k, v in {
            "app": app, "window": window, "max_elements": max_elements,
            "vision": vision, "include_menu": include_menu,
        }.items() if v is not None}
        return self._call("dexter_observe", args)

    def map(self, app: str, wake: Optional[bool] = None) -> dict:
        """Capability map of an app — windows, controls, editable
        fields, menu verbs. The `stage` field reports what the
        policy-gated activation borrow did; `wake=False` maps
        background content only."""
        args: dict = {"app": app}
        if wake is not None:
            args["wake"] = wake
        return self._call("dexter_map", args)

    def candidates(
        self,
        goal: str,
        app: Optional[str] = None,
        max: Optional[int] = None,
    ) -> list:
        """Ranked plausible actions for a goal — you decide which."""
        args: dict = {"goal": goal}
        if app is not None:
            args["app"] = app
        if max is not None:
            args["max"] = max
        return self._call("dexter_candidates", args)

    def act(
        self,
        action: dict,
        app: Optional[str] = None,
        expect: Optional[dict] = None,
    ) -> dict:
        """Policy-gated action execution + verification. `expect` is an
        explicit ExpectedState checked on the post-act world."""
        args: dict = {"action": action}
        if app is not None:
            args["app"] = app
        if expect is not None:
            args["expect"] = expect
        return self._call("dexter_act", args)

    def grant(self, fingerprint: str) -> dict:
        """Grant a `needs_approval` fingerprint — the human path.
        Absent when the server runs with `--no-grants`."""
        return self._call("dexter_grant", {"fingerprint": fingerprint})

    def verify(self, expected: dict, app: Optional[str] = None) -> dict:
        """Check an ExpectedState — VERIFIED / FAILED / UNCERTAIN."""
        args: dict = {"expected": expected}
        if app is not None:
            args["app"] = app
        return self._call("dexter_verify", args)

    def task(
        self,
        goal: str,
        done: dict,
        app: Optional[str] = None,
        max_steps: Optional[int] = None,
        max_secs: Optional[int] = None,
        window: Optional[int] = None,
    ) -> dict:
        """Hand the whole observe→decide→act→verify loop to Dexter."""
        args = {"goal": goal, "done": done}
        if app is not None:
            args["app"] = app
        if max_steps is not None:
            args["max_steps"] = max_steps
        if max_secs is not None:
            args["max_secs"] = max_secs
        if window is not None:
            args["window"] = window
        return self._call("dexter_task", args)

    def cancel(self) -> dict:
        """Cooperatively cancel the in-flight task."""
        return self._call("dexter_cancel")

    def journal(self) -> dict:
        """Live audit trail: {events, dropped}."""
        return self._call("dexter_journal")

    def status(self) -> dict:
        """Runtime status: driver, engine health, journal, task."""
        return self._call("dexter_status")


def _content_text(result: dict) -> str:
    for c in result.get("content", []):
        if c.get("type") == "text":
            return c.get("text", "")
    return ""
