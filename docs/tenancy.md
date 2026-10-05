# Tenancy

Every workspace, snapshot, session, and jail-ledger row belongs to a
tenant. An API key carries a tenant id + a role (`admin` or
`operator`). Operators only see their own tenant's rows; admins see
every tenant. Credentials (the real upstream keys the phantom proxy
resolves) are platform-level — only admins can read or mutate them.

## Key format

Keys live in the `AGENTJAIL_API_KEY` env var as a comma-separated list.
Each entry is:

```
<token>@<tenant>:<role>
```

| component | constraint                                                |
|-----------|------------------------------------------------------------|
| `token`   | any non-empty whitespace-free string                       |
| `tenant`  | `[a-z0-9][a-z0-9_-]{0,63}` — 1–64 chars, URL-safe slug     |
| `role`    | `admin` or `operator`                                      |

### Example

```sh
export AGENTJAIL_API_KEY="
  ak_ops_9f3c@platform:admin,
  ak_acme_alice@acme:operator,
  ak_acme_bob@acme:operator,
  ak_globex_ops@globex:operator
"
```

Every component is mandatory — there are no implicit defaults. A
misconfigured line fails loud rather than silently granting admin.

## Roles

### `admin`

- Sees every tenant's workspaces / snapshots / sessions / jail-ledger rows.
- Sees host paths (`source_dir`, `output_dir`) and bind addresses in
  `GET /v1/config`.
- Can read, add, rotate, and remove credentials.
- Intended for the platform operator, not customer-facing identity.

### `operator`

- Sees only rows stamped with its own `tenant_id`.
- `source_dir` / `output_dir` come back as empty strings.
- Bind addresses + state dir are absent from `GET /v1/config`.
- `GET /v1/credentials` returns `[]`; `POST` / `DELETE` return 404.
- Cross-tenant access uses 404, not 403 — the server never reveals
  whether a row exists outside the caller's tenant.

## Where it lands on the wire

- `GET /v1/whoami` — returns `{ "tenant": "...", "role": "admin"|"operator" }`.
  The dashboard uses this to decide what to render.
- Every list/get/patch/delete on workspaces + snapshots + sessions
  + jails filters by scope. Writes stamp `tenant_id` from the scope.
- Cross-tenant id access always 404s.
- Snapshot rehydrate (`POST /v1/workspaces/from-snapshot`) additionally
  requires `parent_workspace_id` and verifies it matches the snapshot's
  recorded parent — a cheap ownership gate that predates tenancy and
  still adds defense-in-depth.

## Database schema

`0007_tenant_id.sql` adds `tenant_id text NOT NULL DEFAULT 'dev'` to
`workspaces`, `snapshots`, `jails`. Pre-upgrade rows land under `'dev'`,
which is the tenant the control plane emits when auth is disabled — so
stale data stays reachable through the same dev configuration that
produced it.

Indexes are `(tenant_id, created_at DESC)` for the common operator list
shape.

## Accounts page

The UI has an **Accounts** page (`/operator/accounts`) that shows the
caller's resolved tenant + role and a short explainer of how keys are
provisioned. There's no mutation UI yet — add/rotate/revoke is done by
editing `AGENTJAIL_API_KEY` and restarting the control plane. A
DB-backed key store with CRUD endpoints is tracked as the next step.

## Credentials

Upstream keys (OpenAI, Anthropic, GitHub, Stripe) are **per tenant**:

- `POST /v1/credentials` — operators attach their own tenant's key;
  admins attach any tenant's via `?tenant=<id>`.
- `GET /v1/credentials` — operators see their tenant, admins see
  every tenant (or narrow with `?tenant=<id>`).
- `DELETE /v1/credentials/:service` — same scope rules.

On the phantom side:

- `TokenRecord.tenant_id` is stamped when the session mints the
  token (inherited from the session's tenant).
- The proxy forwards by looking up `keys.get(&record.tenant_id,
  record.service)`. A token minted for tenant A cannot spend tenant
  B's credential — even if the services match.
- `InMemoryKeyStore` is keyed by `(tenant, service)`, and
  `PgCredentialStore` has `(tenant_id, service)` as the composite
  primary key after migration `0008_credentials_tenant.sql`.

The `"dev"` tenant is the back-compat sentinel — pre-migration rows
default into it, and `KeyStore::from_env()` also populates `"dev"` so
single-tenant dev deployments keep working with no config.

## URL shape

Every dashboard path is prefixed with `/t/:tenant/`. Examples:

```
/t/acme/projects
/t/acme/operator/ledger
/t/acme/integrations
```

Requests to the old un-prefixed paths (`/projects`, `/operator/…`)
redirect to the caller's own tenant. Operators cannot escalate by
editing the URL — the server's auth scope is the source of truth.
Admins can browse another tenant by editing the URL; the shell
header shows a tenant + role badge so the active tenant is always
visible, and pages like Credentials show a **cross-tenant view**
chip when the URL tenant doesn't match the caller's own.

## What stays single-tenant

- **Audit** rows don't carry `tenant_id` natively. The list endpoint
  filters at query time by joining against session tenancy. Fine for
  current volumes; would need direct stamping at high write rates.

## Tests

`crates/agentjail-ctl/tests/api.rs` covers the HTTP surface:

- `whoami_returns_tenant_and_role`
- `operator_workspace_list_is_tenant_scoped`
- `operator_cannot_read_other_tenants_workspace_by_id`
- `credentials_are_tenant_scoped` — each operator sees only its own;
  admin sees every tenant or filters via `?tenant=`; operator passing
  `?tenant=<other>` 404s.
- `settings_bind_addrs_hidden_from_operators`
- `rename_workspace_cross_tenant_404`
- `flavors_list_reflects_state_dir_contents`
- `flavors_list_requires_auth`

Inline unit tests in `src/tenant.rs`, `src/auth.rs`, `src/workspaces.rs`,
`src/snapshots.rs`, `src/session.rs`, `src/jails.rs` exercise the
filtering primitives.
