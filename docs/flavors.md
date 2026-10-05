# Flavors

A *flavor* is a pre-built read-only directory that ships a runtime
(`nodejs`, `python`, `bun`, ...) + its package manager. The jail
engine bind-mounts the directory into the jail at
`/opt/flavors/<name>/` and auto-prepends `/opt/flavors/<name>/bin` to
`PATH` when that subdir exists. The engine is deliberately oblivious
to "what's inside" — adding deno / elixir / ruby is a matter of
dropping a directory, not changing agentjail code.

This mirrors freestyle.sh's "integration" model: the runtime is
selected per-workspace, the jail stays language-agnostic.

## On-disk layout

Flavors live under `$AGENTJAIL_STATE_DIR/flavors/`:

```
$AGENTJAIL_STATE_DIR/flavors/
  nodejs/
    bin/node, npm, pnpm, ...
    lib/...
  python/
    bin/python3, pip, uv, ...
    lib/...
  bun/
    bin/bun
```

The registry scans this directory on every lookup — add a flavor
while the server is running, it's visible at the next exec.

## Naming rules

`[a-z0-9][a-z0-9_-]{0,31}` — 1–32 chars, lowercase ASCII + digits +
`_-`, not leading `.` or `-`. Unsafe names are skipped with a warning;
they can't appear as path components inside the jail.

## Selecting a flavor

### Via API

```http
POST /v1/workspaces
Content-Type: application/json

{ "flavors": ["nodejs", "python"] }
```

Each name is resolved against the registry at create (typos fail fast
with a 400) and again at every exec (survives flavor renames without
recreating the workspace). Unknown names at exec time fail the exec
with a 400 — the workspace stays usable; the operator fixes the
registry and retries.

### Discovering what's available

```http
GET /v1/flavors
```

Returns `[{ "name": "nodejs" }, { "name": "python" }, ...]`. Any
authenticated caller (admin or operator) can list. Host paths are
admin-internal and never leave the server — the endpoint exposes
names only.

### Via dashboard

The create-workspace form in the projects page calls
`GET /v1/flavors` and renders one toggle chip per registered flavor.
Click to select; selected names go into the `flavors` field on
create. The detail panel shows active flavors under the "Flavors"
section of the technical-config expander.

## Inside the jail

For a workspace requesting `flavors: ["nodejs"]`, the jail sees:

- `/opt/flavors/nodejs/` — read-only bind-mount of the host's
  `$STATE_DIR/flavors/nodejs/`.
- `PATH=/opt/flavors/nodejs/bin:/usr/local/bin:/usr/bin:/bin` — the
  flavor's `bin/` is prepended automatically.

So `bun install`, `npm ci`, `python -m pip install`, etc. all work
without the jail engine knowing what Node or Python is.

## Duplicate basenames

Two overlay paths with the same basename (`/a/nodejs` + `/b/nodejs`)
fail with `JailError::BadConfig` at jail construction. The engine
refuses to silently overlay one on top of the other.

## Clone-jail

Git clone runs inside a short-lived jail by default. Each clone uses:

- strict seccomp
- network allowlist pinned to the repo host only
- 60 s timeout, 512 MB RAM, 64 pid cap
- read-write `/workspace` mounted from the target dir
- no access to anything else on the host

Same config-pin flags as the old host-side path
(`protocol.allow=never`, `core.sshCommand=false`, `fsckObjects=true`,
etc.).

**Fallback:** container runtimes that can't spawn nested namespaces
can opt back into the host path with:

```sh
export AGENTJAIL_CLONE_MODE=host
```

This keeps the full hardening flags in place but runs git directly
on the host process. Any deployment that can already run workspace
exec can also run the clone-jail (same cap requirement), so the
fallback is primarily for heavily-restricted CI/container environments.

## Adding a flavor

The pragmatic path today is to build a minimal rootfs layer and drop
it into the state dir. Rough recipe for nodejs:

```sh
mkdir -p $AGENTJAIL_STATE_DIR/flavors/nodejs/{bin,lib}
# copy node + npm + their shared libs into bin/ and lib/
```

There's no push/pull API yet — flavors are dropped in by the operator
at deploy time. A DB-backed registry with a `POST /v1/admin/flavors`
endpoint is tracked as the next step; at that point the UI gets a
picker on the workspace-create form.

## Tests

`crates/agentjail-ctl/tests/api.rs`:

- `unknown_flavor_rejected_on_create` — 400 on typo, with the missing
  name surfaced in the error body.
- `known_flavor_lands_in_workspace_spec` — plant a directory, create a
  workspace, see the flavor in the stored spec.
- `flavors_list_reflects_state_dir_contents` — plant safe + unsafe
  names + a plain file; only safe subdirectories show up in
  `GET /v1/flavors`; host paths are not in the response.
- `flavors_list_requires_auth` — 401 without a bearer token.

`crates/agentjail-ctl/src/flavors.rs` has inline unit tests for the
registry (missing root, subdir listing, resolution, unsafe-name
filtering).

`crates/agentjail-ctl/src/routes/clone_jail.rs` has inline tests for
the repo-host extractor and the jail-config shape (seccomp=strict,
allowlist=[host], 60s timeout, source_rw).
