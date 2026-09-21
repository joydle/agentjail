# agentjail (Python)

Python client for the [agentjail] control plane. Mirrors the
[@agentjail/sdk][node] TypeScript package.

Python ≥ 3.10. Depends on [httpx].

Tenancy is handled server-side from your API key — there's nothing
to thread through the SDK. Key format on the server:
`token@tenant:role` (see the [root README](../../README.md#tenancy)).
Credentials, sessions, workspaces, snapshots, and the jail ledger
are all scoped to the key's tenant automatically.

```bash
pip install agentjail
```

```python
from agentjail import Agentjail

aj = Agentjail(base_url="http://localhost:7000", api_key="aj_local_...")

aj.credentials.put(service="openai", secret="sk-real")

session = aj.sessions.create(services=["openai"], ttl_secs=600)
# Hand `session["env"]` to whatever sandbox runs your agent.
```

## API

- `credentials.list()` · `put(service, secret)` · `delete(service)`
- `sessions.create(services=, ttl_secs=, scopes=)` · `list()` · `get(id)` · `close(id)`
- `sessions.exec(id, cmd=, args=, …)`
- `runs.create(code=, language=, …)` · `fork(parent_code=, child_code | children=)` · `stream(…)`
- `workspaces.create(…)` · `list()` · `get(id)` · `delete(id)` · `exec(id, cmd=, args=)`
- `snapshots.create(workspace_id, name=)` · `list(…)` · `get(id)` · `manifest(id)` · `delete(id)` · `create_workspace_from(snapshot_id, label=)`
- `audit.recent(limit=)`
- `jails.list(…)` · `jails.get(id)`
- `public.health()` · `public.stats()` — no auth

### Persistent workspaces and snapshots

`git:` is served by the clone-jail: the repo is fetched inside a
short-lived agentjail pinned to the repo host only. No host-side
`git` process ever sees your request.

`flavors:` selects runtime overlays the server has under
`$state_dir/flavors/` — see `GET /v1/flavors` for the live list.
Unknown names 400 at create.

`create_workspace_from(...)` requires `parent_workspace_id` — the
server verifies it matches the snapshot's recorded parent and
returns 404 on mismatch, so nothing leaks about other tenants'
snapshots.

```python
ws = aj.workspaces.create(
    git={"repo": "https://github.com/my-org/app", "ref": "main"},
    flavors=["nodejs", "python"],
    label="ci",
    idle_timeout_secs=60,
)

aj.workspaces.exec(ws["id"], cmd="bun", args=["install"])
baseline = aj.snapshots.create(ws["id"], name="deps-ready")

lint = aj.workspaces.exec(ws["id"], cmd="bun", args=["run", "lint"])
if lint["exit_code"] != 0:
    clean = aj.snapshots.create_workspace_from(
        baseline["id"],
        parent_workspace_id=ws["id"],
        label="recovered",
    )
    # retry against clean["id"]
```

### Streaming

```python
for ev in aj.runs.stream(code="print(42)", language="python"):
    if ev["type"] == "stdout":
        print(ev["line"])
```

### Inbound hostname routing

Workspaces can declare `domains` — the server's gateway forwards
matching `Host:` traffic to a caller-supplied `backend_url` or to a
live jail port via `vm_port`.

```python
aj.workspaces.create(
    label="preview",
    domains=[{"domain": "review-42.preview.local", "backend_url": "http://127.0.0.1:3000"}],
)
```

### Context manager

```python
with Agentjail(base_url=..., api_key=...) as aj:
    aj.public.health()
```

## License

MIT.

[agentjail]: https://github.com/bugthesystem/agentjail
[node]: https://www.npmjs.com/package/@agentjail/sdk
[httpx]: https://www.python-httpx.org/
