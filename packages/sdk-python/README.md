# agentjail (Python)

Python client for the [agentjail](https://github.com/joydle/agentjail)
control plane. Python 3.10 or newer; depends on
[httpx](https://www.python-httpx.org/).

## Install

Not on PyPI (the `agentjail` name there is an unrelated package):

```bash
pip install "git+https://github.com/joydle/agentjail#subdirectory=packages/sdk-python"
```

## Example

```python
import os
import subprocess

from agentjail import Agentjail

with Agentjail(base_url="http://localhost:7070", api_key=os.environ["AGENTJAIL_API_KEY"]) as aj:
    aj.credentials.put(service="openai", secret=os.environ["OPENAI_API_KEY"])

    # Phantom tokens and *_BASE_URL variables for the sandbox; the real
    # key never leaves the server.
    session = aj.sessions.create(services=["openai"], ttl_secs=600)
    subprocess.run(["python", "agent.py"], env={**os.environ, **session["env"]})

    run = aj.runs.create(code="print(6 * 7)", language="python")
    print(run["stdout"])
```

The client mirrors the HTTP API (`credentials`, `sessions`, `runs`,
`workspaces`, `snapshots`, `jails`, `audit`, `settings`, `public`);
see [docs/control-plane.md](../../docs/control-plane.md). Errors are
`AgentjailError` with a stable `code`.

## Development

```bash
uv sync --extra dev && uv run pytest
```
