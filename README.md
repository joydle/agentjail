<p align="center">
  <img src="logo.svg" width="100" height="100" alt="agentjail">
</p>

<h1 align="center">agentjail</h1>

<p align="center">Minimal Linux sandboxes for running untrusted code.</p>

## Why

Model-generated code, `npm install` on a fresh repo, a PR's test
suite, an MCP tool the agent picked — all run as you, on your
kernel. They can read `~/.ssh`, fork-bomb the host, dial any IP,
spawn a reverse shell. Docker isn't a sandbox. `nobody` isn't a
sandbox. agentjail is.

One jail is one child process inside fresh Linux namespaces,
pivot-rooted into a minimal rootfs, seccomp-filtered, cgroup-limited,
egress-allowlisted. No VM. No daemon. No setuid helper.

> Beta. Core crate (`crates/agentjail`) is the load-bearing piece,
> covered by `make test-rust-privileged`. Control plane, SDKs, web
> UI, gateway are useful but APIs may move before 1.0.

## Isolation

- **Namespaces** — mount, network, IPC, PID; user optional.
- **Filesystem** — `pivot_root` to a 128-bit-random tmp root;
  old root `umount2(MNT_DETACH)`-ed. Bind `/bin /lib /usr` ro.
  tmpfs `/etc` with the bare minimum for dynamic linking + DNS.
  Landlock on Linux ≥ 5.13 (hard-fail if requested on older kernels).
- **Network** — `None`, `Loopback`, or `Allowlist(domains)`. Allowlist
  routes through an in-process HTTP CONNECT proxy: resolves the host
  once, rejects private/link-local/loopback/CGNAT, dials the IP not
  the hostname (closes DNS rebinding). Veth via netlink, no `ip` binary.
- **Syscalls** — seccomp-BPF blocklist (`Standard`/`Strict`).
  Blocks namespace, mount, module, keyring, BPF, perf, io_uring,
  `chroot`, `name_to_handle_at`, `ptrace`, `personality`, `clone3`,
  `mount_setattr`, `memfd_create`, `fanotify_init`, `quotactl`,
  `syslog`. Arg-filters `ioctl(*, TIOCSTI, …)` and
  `socket(AF_NETLINK|AF_PACKET|AF_VSOCK, …)`.
- **Privileges** — `PR_SET_NO_NEW_PRIVS`, `close_range(3, ~0, CLOEXEC)`
  before exec, full bounding-set drop, `SECBIT_NOROOT_LOCKED |
  SECBIT_NO_SETUID_FIXUP_LOCKED`, `capset` zeroing every effective /
  permitted / inheritable cap in the grandchild after `/proc` is
  remounted in the new PID namespace.
- **Resources** — memory, CPU, PIDs, disk I/O via cgroup v2. Barrier
  pipe: child blocks until the parent has assigned the cgroup, so
  there's no unconstrained startup window.

## Requirements

- Linux ≥ 5.13, cgroup v2, user namespaces.
- Rust 1.85+ (edition 2024).
- `CAP_NET_ADMIN` — Allowlist mode only (veth + netlink).

## Use

```toml
[dependencies]
agentjail = "0.1"
tokio = { version = "1", features = ["rt", "macros"] }
```

```rust
use agentjail::{Jail, preset_build};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let jail = Jail::new(preset_build("./src", "./out"))?;
    let out  = jail.run("npm", &["run", "build"]).await?;
    println!("exit={} oom={}", out.exit_code, out.oom_killed);
    Ok(())
}
```

### Presets

| Preset | Network | Memory | Timeout |
|---|---|---|---|
| `preset_build`   | None       | 512 MB | 600 s |
| `preset_install` | Allowlist  | 512 MB | 600 s |
| `preset_agent`   | None       | 256 MB | 300 s |
| `preset_gpu`     | None       | 8 GB   | 3600 s |
| `preset_dev`     | Loopback   | 1 GB   | 3600 s |

`preset_install` requires explicit domains:

```rust
preset_install("./src", "./out", vec![
    "registry.npmjs.org".into(),
    "registry.yarnpkg.com".into(),
])
```

### Config

```rust
use agentjail::{Jail, JailConfig, Network, SeccompLevel};

let jail = Jail::new(JailConfig {
    source:        "/code".into(),       // ro at /workspace
    output:        "/artifacts".into(),  // rw at /output
    network:       Network::None,
    seccomp:       SeccompLevel::Standard,
    memory_mb:     512,
    cpu_percent:   100,                  // 100 = 1 core
    max_pids:      64,
    io_read_mbps:  100,
    io_write_mbps: 50,
    timeout_secs:  300,
    ..Default::default()
})?;
```

### Network

```rust
Network::Allowlist(vec![
    "api.anthropic.com".into(),
    "registry.npmjs.org".into(),
    "*.mcp.example.com".into(),
])
```

Hostname checked against the allowlist, resolved, every
private/loopback/link-local/CGNAT/test-net IP filtered, connect to
what's left. TLS passes through (HTTPS, SSE, WebSocket).

### GPU

Trusted workloads only — exposes the NVIDIA kernel-driver attack
surface.

```rust
JailConfig { gpu: GpuConfig { enabled: true, devices: vec![0] },
             ..Default::default() }
```

### Resource monitoring

```rust
let handle = jail.spawn("npm", &["run", "build"])?;
if let Some(s) = handle.stats() {
    println!("mem {} / peak {} MB  pids {}",
        s.memory_current_bytes / 1_048_576,
        s.memory_peak_bytes    / 1_048_576,
        s.pids_current);
}
let out = handle.wait().await?;
if out.oom_killed { eprintln!("OOM"); }
```

### Events

```rust
let (_handle, mut rx) = jail.spawn_with_events("npm", &["run", "build"])?;
while let Some(ev) = rx.recv().await {
    match ev {
        JailEvent::Stdout(l)        => println!("{l}"),
        JailEvent::Stderr(l)        => eprintln!("{l}"),
        JailEvent::OomKilled        => eprintln!("OOM"),
        JailEvent::Completed { .. } => break,
        _ => {}
    }
}
```

### Snapshots and live forks

Save the output dir, restart from it later. Or branch a running jail
without pausing it — useful for running N variants of an agent off
one warm state.

```rust
let snap = Snapshot::create(&output, &snapshot_dir)?;
snap.restore()?;

// FICLONE reflink where supported (btrfs, xfs, ext4-with-reflink);
// regular copy on tmpfs / cross-filesystem. Freezer pauses the source
// jail sub-millisecond for the duration of the clone.
let handle = jail.spawn("python", &["train.py"])?;
let (forked, _info) = jail.live_fork(Some(&handle), "/tmp/fork-out")?;
```

Files are content-addressed by BLAKE3 in a shared object pool;
unchanged files (same `size + mtime_ns`) skip rehashing and reuse
the prior blob. Restore strips `S_ISUID`/`S_ISGID` and rejects
manifest entries with absolute or `..` paths.

## Threat model

Each row links to the regression test that would fail if the
protection ever did. Tests live in
[`crates/agentjail/tests/`](crates/agentjail/tests/).

| Attack | Protection | Test |
|---|---|---|
| Read host `~/.ssh` / `~/.aws` | Not mounted | [`test_cannot_read_ssh_keys`](crates/agentjail/tests/security_test.rs) |
| Read `/etc/shadow`, machine-id | Minimal `/etc` | [`test_etc_shadow_not_accessible`](crates/agentjail/tests/audit_regression_test.rs) |
| Network exfiltration | Netns + allowlist proxy | [`test_network_none_blocks_external`](crates/agentjail/tests/security_test.rs), [`test_reverse_shell_blocked`](crates/agentjail/tests/security_test.rs) |
| Fork bomb | PID limit | [`test_pid_limit_blocks_fork_bomb`](crates/agentjail/tests/audit_regression_test.rs) |
| Memory blow-up | Memory limit + OOM detection | [`test_large_stdout_does_not_oom`](crates/agentjail/tests/audit_regression_test.rs) |
| Disk thrashing | I/O bandwidth limits | [`test_io_write_bandwidth_limit_enforced`](crates/agentjail/tests/audit_regression_test.rs) |
| Signal host processes | PID namespace | [`test_pid_namespace_full_sandbox`](crates/agentjail/tests/security_test.rs) |
| Mount manipulation | `mount` + new mount API blocked | [`seccomp_standard_blocks_documented_syscalls`](crates/agentjail/tests/seccomp_blocklist_test.rs) |
| `chroot` escape | `pivot_root` + detach; `chroot` seccomp-blocked | [`test_chroot_no_home`](crates/agentjail/tests/security_test.rs) |
| io_uring bypass | `io_uring_*` blocked | [`seccomp_standard_blocks_documented_syscalls`](crates/agentjail/tests/seccomp_blocklist_test.rs) |
| Compat-mode escape | `personality()` blocked | [`seccomp_standard_blocks_documented_syscalls`](crates/agentjail/tests/seccomp_blocklist_test.rs) |
| Namespace escape | `clone3`, `unshare`, `setns` blocked | [`test_seccomp_blocks_unshare`](crates/agentjail/tests/audit_regression_test.rs) |
| BPF / perf | `bpf`, `perf_event_open`, `userfaultfd` blocked | [`test_seccomp_blocks_bpf`](crates/agentjail/tests/audit_regression_test.rs) |
| Executable memory | `memfd_create` blocked | [`seccomp_standard_blocks_documented_syscalls`](crates/agentjail/tests/seccomp_blocklist_test.rs) |
| Write + exec on `/tmp` | `NOEXEC` | [`test_tmp_noexec`](crates/agentjail/tests/audit_regression_test.rs) |
| Setuid escalation | `PR_SET_NO_NEW_PRIVS` | — |
| Core-dump leak | `RLIMIT_CORE=0` | [`test_rlimit_core_disabled`](crates/agentjail/tests/audit_regression_test.rs) |
| Parent stdout OOM | Output capped at 256 MiB / stream | [`test_large_stdout_does_not_oom`](crates/agentjail/tests/audit_regression_test.rs) |
| FD exhaustion | `RLIMIT_NOFILE` at 4096 | [`test_fd_limit_enforced`](crates/agentjail/tests/audit_regression_test.rs) |
| Symlink traversal | Skipped in snapshots, forks, cleanup | [`test_snapshot_restore_does_not_follow_symlinks`](crates/agentjail/tests/audit_regression_test.rs) |
| Zombie / fd leak | `PR_SET_PDEATHSIG` + Drop kills+reaps | [`test_no_zombie_after_drop`](crates/agentjail/tests/audit_regression_test.rs) |
| Cross-tenant read | `tenant_id` on every row; list filters, get 404s | [`operator_cannot_read_other_tenants_workspace_by_id`](crates/agentjail-ctl/tests/api.rs), [`credentials_are_tenant_scoped`](crates/agentjail-ctl/tests/api.rs) |
| Token spent on another tenant's bill | `TokenRecord.tenant_id`; proxy looks up `keys.get(tenant, service)` | [`agentjail-phantom`](crates/agentjail-phantom/src/proxy.rs) |
| Malicious `.gitmodules` / `core.sshCommand` | Clone-jail: strict-ish seccomp, allowlist net, no host access | [`clone_jail_clones_a_small_public_repo`](crates/agentjail-ctl/tests/clone_jail_test.rs) |
| Operator reads platform internals via `GET /v1/config` | Admin-only fields omitted for operators | [`settings_bind_addrs_hidden_from_operators`](crates/agentjail-ctl/tests/api.rs) |
| Snapshot rehydrate spoofing | Requires `parent_workspace_id`, verified | [`from_snapshot_requires_and_checks_parent_workspace_id`](crates/agentjail-ctl/tests/api.rs) |

## Limits

- Linux only. Not a VM — a kernel exploit escapes. For stronger
  isolation, pair with [gVisor](https://gvisor.dev) or run inside
  [Firecracker](https://firecracker-microvm.github.io).
- GPU mode widens the attack surface to the NVIDIA driver.
- Allowlist mode costs one veth pair per concurrent jail. Stale
  interfaces are reaped at `agentjail-server` startup.

## Control plane

Optional. The library is enough for one process. The server is for
when you have many: shared upstream credentials, a workspace ledger,
snapshots, an SSE feed of every API call, a UI.

```bash
# token@tenant:role  — every component required, no defaults.
export AGENTJAIL_API_KEY="\
  ak_ops@platform:admin,\
  ak_acme_alice@acme:operator,\
  ak_globex_ops@globex:operator"
docker compose -f docker-compose.platform.yml up --build
# UI:  http://localhost:3000/t/<tenant>
# API: http://localhost:7000
```

### Tenancy

Every workspace, snapshot, session, jail-row, and credential is
stamped with `tenant_id`. Operators see their own tenant. Admins
see all, with `?tenant=<id>` to scope. Cross-tenant id access
returns 404, never 403 — the server doesn't reveal whether a row
outside scope exists. Full key format and DB shape:
[`docs/tenancy.md`](docs/tenancy.md).

### Phantom credentials

Sandboxes never see real upstream keys. Sessions hand out phantom
tokens (`phm_<hex>`) plus `*_BASE_URL` env vars pointing at the
proxy; the proxy swaps the token for the real key on the way out.
Per-tenant: a token minted for tenant A can't spend tenant B's
credentials even if the service matches.

### Flavors

Runtime "flavors" (`nodejs`, `python`, `bun`, …) are host
directories under `$state_dir/flavors/<name>/`, bind-mounted ro
into each jail at `/opt/flavors/<name>/`, with `bin/` prepended to
`PATH`. Adding `deno` is a `mkdir`, not a code change.

```json
POST /v1/workspaces
{ "flavors": ["nodejs", "python"] }
```

`GET /v1/flavors` lists names (host paths stay admin-internal).
See [`docs/flavors.md`](docs/flavors.md).

### Clone-jail

`git clone` runs in its own short-lived jail by default — strict
seccomp, network allowlist pinned to the repo host, 60 s timeout,
no host access. A malicious `.gitmodules` or `core.sshCommand`
can't reach anything off the target dir. Opt out on restricted
container runtimes:

```bash
export AGENTJAIL_CLONE_MODE=host   # default: jail
```

### Surface

- Identity — `GET /v1/whoami` · `GET /v1/flavors`
- Credentials (per-tenant) — `POST` · `GET` · `DELETE /v1/credentials/:service` (admins: `?tenant=<id>`)
- Sessions — `POST /v1/sessions` · `POST /v1/sessions/:id/exec`
- Runs — `POST /v1/runs` · `/fork` · `/stream`
- Workspaces — `POST /v1/workspaces` · `/fork` · `/exec` · `PATCH` · `POST /v1/workspaces/:id/snapshot` · `POST /v1/workspaces/from-snapshot` (requires `parent_workspace_id`)
- Lists (tenant-filtered) — `GET /v1/{workspaces,snapshots,sessions,jails,audit}`
- Detail — `GET /v1/snapshots/:id/manifest` · `GET /v1/jails/:id` · `GET /v1/config` (bind-addrs + state_dir admin-only)

### Web UI

![control plane](media/control-plane.png)

React 19 + Vite + Tailwind. Pages live at `/t/:tenant/...` so the
active tenant is bookmarkable. Pages: Dashboard, Projects, API
Sessions, Integrations, Playground, Docs. Operator tools behind
`Advanced`: Execution Ledger, Snapshots, API Audit, Accounts,
System Settings.

### SDKs

Node ([`@agentjail/sdk`](packages/sdk-node/README.md), zero deps,
Node ≥ 18) and Python ([`agentjail`](packages/sdk-python/README.md),
≥ 3.10, depends on `httpx`). Symmetrical surface.

```ts
import { Agentjail } from "@agentjail/sdk";

const aj = new Agentjail({
  baseUrl: "http://localhost:7000",
  apiKey:  process.env.AGENTJAIL_API_KEY!,
});

await aj.credentials.put({ service: "openai", secret: process.env.OPENAI_API_KEY! });

for await (const ev of aj.runs.stream({ code, language: "python" })) {
  if (ev.type === "stdout") process.stdout.write(ev.line + "\n");
}

const session = await aj.sessions.create({
  services: ["openai", "github"],
  scopes:   { github: ["/repos/my-org/*"] },
  ttlSecs:  600,
});
spawn("node", ["agent.js"], { env: { ...process.env, ...session.env } });
```

## Build and test

```bash
make test-rust                     # low-priv unit slice (Docker)
make test-rust-privileged          # full security suite (--privileged)
make test-rust-privileged-clone    # end-to-end clone-jail + workspace-exec
( cd packages/sdk-node    && npm test )
( cd packages/sdk-python  && pytest )
( cd web && npm run build )
```

GPU tests need an NVIDIA GPU + the
[Container Toolkit](https://docs.nvidia.com/datacenter/cloud-native/container-toolkit/install-guide.html):

```bash
docker compose run --rm gpu cargo test --test gpu_test -- --nocapture
```

## License

MIT. See [LICENSE](LICENSE).
