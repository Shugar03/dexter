# dexter-python

Thin, dependency-free Python SDK over the `dexter mcp` stdio server —
one `import` away from computer use for any Python agent or harness.

```python
from dexter import Dexter

with Dexter() as d:
    obs = d.observe(app="TextEdit")          # windows[], elements[], digest
    cands = d.candidates("save the document") # ranked menu — you decide
    result = d.act(cands[0]["action"])        # policy-gated + verified

    # Or hand the whole loop to Dexter:
    d.task("save the document",
           done={"element_exists": {"name": "Guardado"}},
           max_secs=120)
```

## Trust flags are operator-side

They belong to the server command, not to calls:

```python
Dexter(args=("mcp", "--approve-all"))              # pre-grant mutations
Dexter(args=("mcp", "--coords"))                    # open physical tier
Dexter(args=("mcp", "--engine", "laya",
             "--engine-path", "python3 workers/laya/worker.py"))
Dexter(args=("mcp", "--driver", "browser",
             "--browser-url", "http://localhost:9515"))
```

An agent payload cannot escalate itself — `approve`/`coords` fields in
tool arguments are ignored by design.

## Tests

```
python3 test_dexter.py            # fake-server contract tests
DEXTER_BIN=/path/to/dexter python3 test_dexter.py   # + real server smoke
```
