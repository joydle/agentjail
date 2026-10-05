# Demos

Runnable scripts against a live control plane. Each mirrors a
well-known product pattern using the real
[`@agentjail/sdk`](../../packages/sdk-node/).

| Script | Pattern | Key features |
|---|---|---|
| [`1-ai-assistant.ts`](1-ai-assistant.ts)         | OpenClaw / Claude / Cowork | persistent workspace, idle auto-pause |
| [`1-ai-assistant.py`](1-ai-assistant.py)         | Python port of #1          | same surface, via the Python SDK |
| [`2-review-bot.ts`](2-review-bot.ts)             | Code Rabbit / Greptile     | git clone, multi-exec, branch on output |
| [`3-background-agent.ts`](3-background-agent.ts) | Devin / Cursor Agent       | N-way workspace fork, parallel execs |
| [`4-app-builder.ts`](4-app-builder.ts)           | Lovable / Bolt / V0        | dev server + hostname-routed gateway |

## Run

```bash
make dev   # start the stack if it isn't already up

bun scripts/demos/1-ai-assistant.ts
bun scripts/demos/2-review-bot.ts
bun scripts/demos/3-background-agent.ts
bun scripts/demos/4-app-builder.ts

# Python variant (requires Python ≥ 3.10)
uv run --with ./packages/sdk-python scripts/demos/1-ai-assistant.py
```

### Environment

- `CTL_URL` — control plane base URL (default `http://localhost:7070`)
- `AGENTJAIL_API_KEY` — bearer token from `.env.local`
- `DEMO_REPO` / `DEMO_REF` — override the repo the git-based demos clone

## `ai(vm, prompt)` stand-in

All four reuse [`_client.ts`](./_client.ts). Its `ai` helper just
echoes the prompt. Real deployments wire phantom tokens through a
session and run a script that calls Claude (or similar) inside the
jail — the loop shape doesn't change.

## Gateway forwarding — demo #4

`domains: [{ domain, vm_port }]` maps a hostname to a port bound
inside the workspace's jail. The gateway resolves the port to
`http://<live_jail_ip>:<vm_port>/` at request time — no `backend_url`,
no sidecar, no socat or ngrok. Requires
`network: { mode: "allowlist", domains: [...] }` so the veth pair
gets created.

When no exec is in flight (dev server not started, or previous exec
exited), the gateway returns `503 Service Unavailable`. Start the
dev server backgrounded inside a long-running exec; subsequent
requests land.
