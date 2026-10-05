# @agentjail/sdk

TypeScript client for the [agentjail] control plane. Zero deps, Node
≥ 18 (or any runtime with a global `fetch`).

Sandboxes never see real API keys. A session returns an `env` object
containing phantom tokens (`phm_<hex>`) plus `*_BASE_URL` entries
pointing at the phantom proxy; the proxy swaps the phantom for the
real key at request time.

Tenancy is handled server-side from your API key — there's nothing
to thread through the SDK. Key format on the server:
`token@tenant:role` (see the [root README](../../README.md#tenancy)).
Credentials, sessions, workspaces, snapshots, and the jail ledger
are all scoped to the key's tenant automatically. Admins targeting
a specific tenant use the control plane directly via
`?tenant=<id>` query params — not yet exposed through the SDK.

```ts
import { Agentjail } from "@agentjail/sdk";

const aj = new Agentjail({
  baseUrl: "http://localhost:7000",
  apiKey:  process.env.AGENTJAIL_API_KEY!,
});

await aj.credentials.put({ service: "openai", secret: "sk-real" });

const session = await aj.sessions.create({
  services: ["openai"],
  ttlSecs:  600,
});

// Hand `session.env` to whatever sandbox you use:
spawn("node", ["my-agent.js"], { env: { ...process.env, ...session.env } });
```

## API

- `credentials.list()` · `put({service, secret})` · `delete(service)`
- `sessions.create({services, scopes?, ttlSecs?})` · `list()` · `get(id)` · `close(id)`
- `sessions.exec(id, {cmd, args?, …})`
- `runs.create({code, language?, …})` · `fork({parentCode, childCode | children})` · `stream(…)`
- `workspaces.create(…)` · `list()` · `get(id)` · `delete(id)` · `exec(id, {cmd, args?})`
- `snapshots.create(workspaceId, {name?})` · `list(…)` · `get(id)` · `manifest(id)` · `delete(id)` · `createWorkspaceFrom(snapshotId, {label?})`
- `audit.recent(limit?)`
- `jails.list(…)` · `jails.get(id)`
- `public.health()` · `public.stats()` — no auth

Supported services: `openai`, `anthropic`, `github`, `stripe`.

### Scopes

Per-service allow-list of path globs (trailing `*` supported).
Out-of-scope requests return 403 at the proxy — the upstream is never
contacted.

```ts
await aj.sessions.create({
  services: ["github"],
  scopes:   { github: ["/repos/my-org/*/issues*"] },
});
```

### Workspaces and snapshots

Workspaces are long-lived mount trees. Multiple execs share the same
filesystem — `bun install` in one call persists for the next.
Snapshots capture the workspace's output dir; rehydrate into a fresh
workspace with `createWorkspaceFrom(snapshotId, { parentWorkspaceId,
label? })` — `parentWorkspaceId` is a required ownership proof (the
snapshot's recorded parent must match).

`git:` is served by the clone-jail: the repo is fetched inside a
short-lived agentjail pinned to the repo host only. No host-side
`git` process ever sees your request.

`flavors:` selects runtime overlays the server has under
`$state_dir/flavors/` — see `GET /v1/flavors` for the live list.
Unknown names 400 at create.

```ts
const ws = await aj.workspaces.create({
  git:     { repo: "https://github.com/my-org/app", ref: "main" },
  flavors: ["nodejs", "python"],
  label:   "ci",
});

await aj.workspaces.exec(ws.id, { cmd: "bun", args: ["install"] });
const baseline = await aj.snapshots.create(ws.id, { name: "deps-ready" });

const lint = await aj.workspaces.exec(ws.id, { cmd: "bun", args: ["run", "lint"] });
if (lint.exit_code !== 0) {
  const clean = await aj.snapshots.createWorkspaceFrom(baseline.id, {
    label: "recovered",
  });
  // retry against clean.id
}
```

Snapshots taken mid-run freeze the jail's cgroup around the copy
(sub-ms on cgroup v2). The incremental path uses a content-addressed
pool keyed by BLAKE3; `manifest(id)` returns the per-file hash list.

[agentjail]: https://github.com/bugthesystem/agentjail
