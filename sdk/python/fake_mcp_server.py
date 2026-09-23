"""Fake MCP server for SDK tests — newline-delimited JSON-RPC over
stdio. Answers `initialize`, echoes canned payloads for known tools,
interleaves a server notification before each response, and supports
a `never_respond` tool for the timeout test.
"""
import json
import sys

ANSWERS = {
    "dexter_observe": {"observation": 1, "windows": [], "elements": [], "digest": "sim"},
    "dexter_status": {"driver": {"name": "fake"}, "engine": {"name": "rule-based", "health": {"status": "ready"}}, "journal": {"events": 0, "dropped": 0}, "task_running": False},
    "dexter_journal": {"events": [], "dropped": 0},
    "dexter_cancel": {"cancelled": False},
}

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    msg = json.loads(line)
    method = msg.get("method", "")
    rid = msg.get("id")

    if method == "initialize":
        out = {"jsonrpc": "2.0", "id": rid, "result": {
            "protocolVersion": "2025-06-18",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "fake", "version": "0"},
        }}
    elif method == "notifications/initialized":
        continue
    elif method == "tools/call":
        name = msg["params"]["name"]
        if name == "never_respond":
            continue
        if name == "fails":
            out = {"jsonrpc": "2.0", "id": rid, "result": {
                "content": [{"type": "text", "text": "boom"}],
                "isError": True,
            }}
        elif name in ANSWERS:
            # Interleave a notification before the response — the SDK
            # must skip it.
            sys.stdout.write(json.dumps({
                "jsonrpc": "2.0", "method": "notifications/message",
                "params": {"level": "info", "data": "chatter"},
            }) + "\n")
            sys.stdout.flush()
            out = {"jsonrpc": "2.0", "id": rid, "result": {
                "content": [{"type": "text", "text": json.dumps(ANSWERS[name])}],
            }}
        else:
            out = {"jsonrpc": "2.0", "id": rid, "error": {
                "code": -32601, "message": f"unknown tool {name}",
            }}
    else:
        out = {"jsonrpc": "2.0", "id": rid, "error": {
            "code": -32601, "message": f"unknown method {method}",
        }}

    sys.stdout.write(json.dumps(out) + "\n")
    sys.stdout.flush()
