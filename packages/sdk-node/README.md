# @agentjail/sdk

TypeScript client for the [agentjail](https://github.com/joydle/agentjail)
control plane. No runtime dependencies; Node 18 or newer (any runtime
with a global `fetch`).

## Install

Not published to npm yet. Build it from the repository:

```bash
cd packages/sdk-node && npm ci && npm run build
npm install /path/to/agentjail/packages/sdk-node   # in your project
```

## Example

```ts
import { spawn } from "node:child_process";
import { Agentjail } from "@agentjail/sdk";

const aj = new Agentjail({
  baseUrl: "http://localhost:7070",
  apiKey: process.env.AGENTJAIL_API_KEY!,
});

await aj.credentials.put({ service: "openai", secret: process.env.OPENAI_API_KEY! });

// Phantom tokens and *_BASE_URL variables for the sandbox; the real
// key never leaves the server.
const session = await aj.sessions.create({ services: ["openai"], ttlSecs: 600 });
spawn("node", ["agent.js"], { env: { ...process.env, ...session.env } });

const run = await aj.runs.create({ code: "print(6 * 7)", language: "python" });
console.log(run.stdout);
```

The client mirrors the HTTP API (`credentials`, `sessions`, `runs`,
`workspaces`, `snapshots`, `jails`, `audit`, `settings`, `public`);
see [docs/control-plane.md](../../docs/control-plane.md). Errors are
`AgentjailError` with a stable `code`.

## Development

```bash
npm ci && npm run typecheck && npm test
```
