# Control plane

`agentjail-server` runs the control plane (`agentjail-ctl`, the HTTP API
on `CTL_ADDR`), the phantom-credential proxy (`agentjail-phantom`, on
`PROXY_ADDR`) and, optionally, the hostname-routed gateway. It is for
deployments with many callers: shared upstream credentials, persistent
workspaces, snapshots, a ledger of every run, an audit feed and a web
UI. A single process that only needs sandboxes can use the `agentjail`
crate directly.

Every environment variable the server reads is listed at the top of
[`crates/agentjail-server/src/main.rs`](../crates/agentjail-server/src/main.rs).

## Running it

```bash
make setup                      # writes .env.local: API key + Postgres password
set -a; source .env.local; set +a
docker compose -f docker-compose.platform.yml up --build
# UI:  http://localhost:3000/t/<tenant>
# API: http://localhost:7070
```

Defaults:

- The server refuses to start unless `AGENTJAIL_API_KEY` holds at least
  one valid `token@tenant:role` entry. `AGENTJAIL_DEV_INSECURE=1` starts
  it without keys for local development; every caller is then a `dev`
  admin.
- Listeners bind `127.0.0.1`. The compose file publishes on `127.0.0.1`
  unless `AGENTJAIL_BIND` says otherwise, never publishes Postgres, and
  has no default Postgres password.
- Jails run with a user namespace and Landlock. The server logs a warning
  when `AGENTJAIL_USER_NAMESPACE=0` or `AGENTJAIL_LANDLOCK=0`.
- Without `DATABASE_URL` all state is in memory and lost on restart.

## API keys and tenancy

`AGENTJAIL_API_KEY` is a comma-separated list of `token@tenant:role`
entries. Every component is required; a malformed entry fails startup
instead of granting access.

| Component | Constraint |
|---|---|
| `token` | non-empty, no whitespace |
| `tenant` | `[a-z0-9][a-z0-9_-]{0,63}` |
| `role` | `admin` or `operator` |

```sh
export AGENTJAIL_API_KEY="ak_ops@platform:admin,ak_acme_alice@acme:operator,ak_globex@globex:operator"
```

Workspaces, snapshots, sessions, jail-ledger rows and credentials carry
a `tenant_id`, set from the caller's key on write.

- **operator**: sees only its own tenant's rows. Host paths
  (`source_dir`, `output_dir`) come back empty, and bind addresses and
  the state dir are left out of `GET /v1/config`.
- **admin**: sees every tenant, and can narrow any list or credential
  call with `?tenant=<id>`.
- A request for another tenant's row returns 404, not 403, so the
  server doesn't reveal whether it exists.
- `GET /v1/whoami` returns `{"tenant": "...", "role": "admin"|"operator"}`.
- Audit rows have no `tenant_id` column; the list endpoint filters them
  through the owning session's tenant.

Keys are changed by editing `AGENTJAIL_API_KEY` and restarting; the
UI's Accounts page only shows the caller's tenant and role. UI paths are
prefixed with `/t/<tenant>/`; the server's auth scope, not the URL,
decides what an operator can see.

Migration `0007_tenant_id.sql` added `tenant_id` with default `'dev'`,
the tenant used when auth is disabled, so older rows stay reachable
from a dev setup. List queries are indexed on `(tenant_id, created_at
DESC)`.

## Credentials and phantom tokens

Real upstream keys (OpenAI, Anthropic, GitHub, Stripe) are stored per
tenant with `POST /v1/credentials`, listed with `GET`, and removed with
`DELETE /v1/credentials/:service`. They never reach a sandbox.

`POST /v1/sessions` returns an `env` map: phantom tokens (`phm_<hex>`)
and `*_BASE_URL` variables pointing at the proxy. The proxy looks up
the token, checks the session's path scopes (per-service globs with a
trailing `*`; out-of-scope requests get 403 without contacting the
upstream), and forwards with the real key of the token's tenant. A
token minted for tenant A can't spend tenant B's key for the same
service. The proxy doesn't follow upstream redirects.

## Workspaces, snapshots and forks

A workspace is a persistent source/output tree; each `exec` runs a new
jail over it, so `npm install` in one call is still there in the next.
`POST /v1/workspaces/:id/snapshot` captures the output dir, freezing a
running exec's cgroup for the copy (the snapshot fails rather than
copying a live tree it can't freeze). `POST /v1/workspaces/from-snapshot`
creates a workspace from a snapshot and requires `parent_workspace_id`,
which must match the snapshot's recorded parent.

With `AGENTJAIL_SNAPSHOT_POOL_DIR` set, snapshots are incremental:
file bodies go into a BLAKE3 content-addressed pool shared by all
snapshots, and `GET /v1/snapshots/:id/manifest` lists the entries.
`AGENTJAIL_SNAPSHOT_MAX_AGE_SECS` / `_MAX_COUNT` drive garbage
collection. Idle workspaces (`idle_timeout_secs`) are paused, with an
optional automatic snapshot.

`POST /v1/runs/fork` and `POST /v1/workspaces/:id/fork` branch a
running jail with `live_fork`: reflink copies where the filesystem
supports them, otherwise a byte copy.

## Flavors

A flavor is a read-only runtime directory (`nodejs`, `python`, `bun`,
...) under `$AGENTJAIL_STATE_DIR/flavors/<name>/`. A workspace created
with `"flavors": ["nodejs", "python"]` gets each one bind-mounted
read-only at `/opt/flavors/<name>/`, with its `bin/` prepended to
`PATH`. The engine doesn't know what is inside; adding a runtime means
adding a directory.

- Names match `[a-z0-9][a-z0-9_-]{0,31}`; other directories are skipped
  with a warning.
- The directory is scanned on every lookup, so new flavors appear
  without a restart.
- Names are checked at create (unknown: 400) and again at each exec.
- `GET /v1/flavors` lists names for any authenticated caller; host
  paths are not exposed.
- Two overlays with the same basename are a `BadConfig` error.

The server image seeds `nodejs`, `python` and `bun` with
[`scripts/seed-flavors.sh`](../scripts/seed-flavors.sh). There is no
upload API.

## Clone jail

A workspace's `git` seed is cloned inside a short-lived jail: Standard
seccomp (Strict blocks the sockets git needs), a network allowlist of
the repo host, a 60 s timeout, 512 MB and 64 pids, the target dir as a
read-write `/workspace`, and git's hardening flags
(only the https protocol, `core.sshCommand=false`,
`transfer.fsckObjects=true`, ...). A malicious `.gitmodules` or
`core.sshCommand` reaches nothing but the target dir.
`AGENTJAIL_CLONE_MODE=host` runs git on the host with the same flags,
for container runtimes that can't create nested namespaces.

## Gateway

With `AGENTJAIL_GATEWAY_ADDR` set, the server also forwards HTTP by
`Host` header to a workspace's declared `domains`: either a
`backend_url`, or `vm_port`, a port on the workspace's running jail
(503 while no exec is live). Hostnames are unique across tenants.
`backend_url` targets may only reach public addresses
(`AGENTJAIL_ALLOW_PRIVATE_BACKENDS=1` lifts that for development); the
check runs in the HTTP client's resolver, so DNS rebinding can't get
around it. Requests are capped at 10 MB, responses at 32 MB, and at
`AGENTJAIL_GATEWAY_MAX_INFLIGHT` (default 64) at once. There is no TLS;
terminate it in front.

## API

- Identity: `GET /v1/whoami`, `GET /v1/flavors`
- Credentials: `POST`, `GET /v1/credentials`, `DELETE /v1/credentials/:service`
- Sessions: `POST /v1/sessions`, `GET /v1/sessions[/:id]`, `DELETE /v1/sessions/:id`, `POST /v1/sessions/:id/exec`
- Runs: `POST /v1/runs`, `/v1/runs/fork`, `/v1/runs/stream` (SSE)
- Workspaces: `POST`, `GET`, `PATCH`, `DELETE /v1/workspaces[/:id]`, `POST /v1/workspaces/:id/{exec,fork,snapshot}`, `POST /v1/workspaces/from-snapshot`
- Snapshots: `GET /v1/snapshots[/:id]`, `GET /v1/snapshots/:id/manifest`, `DELETE /v1/snapshots/:id`
- Ledger and audit: `GET /v1/jails[/:id]`, `GET /v1/audit`
- Server: `GET /v1/config`; `GET /healthz` and `GET /v1/stats` need no key

The Node ([`packages/sdk-node`](../packages/sdk-node)) and Python
([`packages/sdk-python`](../packages/sdk-python)) clients cover the
same surface.

## Web UI

`web/` is a React + Vite app served on port 3000 by the compose stack.
Pages live under `/t/<tenant>/`: Dashboard, Projects, API Sessions,
Integrations, Playground, and, under Advanced, Execution Ledger,
Snapshots, API Audit, Accounts and System Settings.

## Tests

`crates/agentjail-ctl/tests/api.rs` covers the HTTP surface, including
tenant scoping (`operator_cannot_read_other_tenants_workspace_by_id`,
`credentials_are_tenant_scoped`, `settings_bind_addrs_hidden_from_operators`)
and flavors (`unknown_flavor_rejected_on_create`,
`flavors_list_reflects_state_dir_contents`). The Postgres store tests
run when `AGENTJAIL_TEST_DATABASE_URL` is set.
`crates/agentjail-ctl/tests/clone_jail_test.rs` clones a real repository
in a jail (`--ignored`, needs root).
