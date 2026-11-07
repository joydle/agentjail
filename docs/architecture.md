# agentjail architecture

Contributor docs. How the jail actually works, and what you have to keep
straight in your head when you change code.

## Overview

A jail is a sandboxed Linux child process. The parent `fork()`s, the
child enters fresh namespaces (`user` and `ipc` by default, `mount`,
`pid`, `net`), builds a minimal rootfs on a private tmpfs mounted at an
unpredictable `/tmp/agentjail-<128-bit-random>` (bind mounts into it,
then read-only), `pivot_root`s + detaches the old root, applies Landlock, drops privileges (caps, securebits, seccomp
BPF), and `execve()`s
the target command. The parent collects stdout/stderr over pipes and
waits on a pidfd. Optional cgroup-v2 limits gate memory, CPU, PIDs,
and I/O.

For Allowlist networking, the parent also sets up a veth pair, runs a
small HTTP-CONNECT proxy on the host side, and the child routes through
it via `HTTP_PROXY` env vars. A host nftables table (`veth/firewall.rs`)
drops anything else the jail sends to the host or routes through it.

## One jail, end to end

1. `Jail::new(config)` validates paths, discovers GPU resources, and
   **compiles the seccomp BPF program** — kept in an `Arc<BpfProgram>`
   so every subsequent `spawn()` applies the cached filter instead of
   rebuilding.
2. `jail.spawn(cmd, args)`:
   1. Parent creates two output pipes (stdout, stderr) and a **barrier**
      socketpair; with a user namespace, a userns socketpair; for
      Allowlist, a netns socketpair and a reserved `VethPair` (id +
      addresses; nothing on the host yet).
   2. Parent `fork()`s, on the internal "agentjail-spawner" thread
      ([`spawner.rs`](../crates/agentjail/src/spawner.rs)); the rest of
      the setup continues on the calling thread.
   3. **Child** sets `PR_SET_PDEATHSIG(SIGKILL)`, clears its signal
      mask, `setsid()`, `dup2()`s
      the pipe write ends onto stdout/stderr, then **blocks on
      `read(barrier)`**. It does not move until the parent signals.
   4. **Parent** creates and configures the cgroup (writes
      `memory.max`, `cpu.max`, `pids.max`, optionally `io.max`), adds
      the child's PID to `cgroup.procs`, then writes one byte to the
      barrier. The child can now proceed without a resource-limit
      bypass window.
   5. **Parent** — user namespace only — waits for the child's
      `unshare(NEWUSER)`, writes `uid_map`/`gid_map` for that namespace
      (jail 0 → an unprivileged host id), and releases it.
   6. **Parent** — Allowlist only — waits for the child to signal "I'm
      in my netns", then `VethPair::create` → `attach` (jail end into
      the child's netns, host end addressed + up) → host firewall →
      proxy thread (bound to the host end, serving only the jail's
      address), and releases the child. Any failure fails the spawn:
      the child is killed and reaped and dropping the `VethPair`
      deletes the link and frees its id. Every wait has a deadline and
      a child that dies is seen as EOF, so `spawn` never hangs.
   7. **Child** (`exec::run_child`): userns handshake →
      `unshare(NEWNS|NEWNET|NEWIPC)` → configure network → mount
      rootfs → `RLIMIT_NOFILE/CORE` + `PR_SET_NO_NEW_PRIVS` →
      `close_range(CLOEXEC)` → PID namespace double-fork → in the
      jail's init: mount + mask `/proc` → `pivot_root` + detach old root
      → Landlock → switch to `run_as` (no-userns root embedder only),
      drop all capabilities + lock securebits → apply cached seccomp → `execve()`. Any failed step
      `_exit`s with a message on the jail's stderr (fail closed).
3. `handle.wait().await`:
   1. Moves stdout/stderr out of the handle into background drain
      tasks. **Drains concurrently** with the process wait — the child
      must never block on a full pipe.
   2. `wait_for_pid(pid)` opens a `pidfd_open(pid, 0)` and uses tokio
      `AsyncFd::readable()` for an edge-triggered wakeup at exit. No
      polling on Linux 5.3+; graceful 50 ms poll fallback.
   3. Collects cgroup stats, marks `reaped=true` so `Drop` doesn't kill
      a PID-recycled process, stops the proxy and drops the `VethPair`.

## Crate map

| File | Owns |
|---|---|
| [`run.rs`](../crates/agentjail/src/run.rs) | `Jail`, `JailHandle`, `Output`. Lifecycle, fork, parent-side orchestration. |
| [`exec.rs`](../crates/agentjail/src/exec.rs) | Everything that runs **after fork, before exec**, inside the child. |
| [`run_internal.rs`](../crates/agentjail/src/run_internal.rs) | `wait_for_pid` (pidfd + AsyncFd + poll fallback), `kill_tree`, exit-code translation. |
| [`pipe.rs`](../crates/agentjail/src/pipe.rs) | `Pipe` + `OutputStream`. stdout/stderr read via `tokio::net::unix::pipe::Receiver` (epoll, not the blocking pool). |
| [`cgroup.rs`](../crates/agentjail/src/cgroup.rs) | cgroup-v2 setup, limits, stats, freezer. `freeze()` polls `cgroup.events` until `frozen 1`. |
| [`namespace.rs`](../crates/agentjail/src/namespace.rs) | `unshare`, `uid_map`/`gid_map` write, loopback ioctl. |
| [`mount.rs`](../crates/agentjail/src/mount.rs) | Bind mounts, tmpfs, `/proc`, minimal `/dev`, `/etc` whitelist, `make_root_private`. |
| [`seccomp.rs`](../crates/agentjail/src/seccomp.rs) | BPF filter compile + apply. `CompiledFilter` is built once in `Jail::new`. |
| [`fork.rs`](../crates/agentjail/src/fork.rs) | `live_fork` — cow_clone through snapshot's no-follow copy (`FICLONE`, byte-copy fallback, setuid stripped). |
| [`snapshot.rs`](../crates/agentjail/src/snapshot.rs) | Plain + content-addressed incremental snapshots, manifest format, freezer wait, mtime+size fast-path. |
| [`netlink.rs`](../crates/agentjail/src/netlink.rs) | Raw `NETLINK_ROUTE` — veth create, ifindex, addr, route, link up. No iproute2 dependency. |
| [`veth.rs`](../crates/agentjail/src/veth.rs) | `IdAllocator` + RAII `VethPair`, IP derivation, fallible proxy launch, leak sweep; `veth/firewall.rs` installs the nftables table via `nft`. |
| [`proxy.rs`](../crates/agentjail/src/proxy.rs) | HTTP CONNECT proxy (pre-parsed allowlist, wildcards). Two logical phases: request parse, bidirectional tunnel. |
| [`gpu.rs`](../crates/agentjail/src/gpu.rs) | NVIDIA device + library discovery and bind-mount. |
| [`landlock.rs`](../crates/agentjail/src/landlock.rs) | Filesystem ACLs (Linux 5.13+), on by default, applied after `pivot_root`; errors if not enforced. |
| [`config.rs`](../crates/agentjail/src/config.rs) | `JailConfig`, `SeccompLevel`, `Network`, presets. |
| [`error.rs`](../crates/agentjail/src/error.rs) | `JailError` enum, `Result` alias. |
| [`events.rs`](../crates/agentjail/src/events.rs) | Simple mpsc channel for lifecycle events. |

## Invariants

These are the hard rules. If you edit a function that touches any of
them, read the full context first.

### 1. Barrier pipe stays in place.

The child reads one byte before doing anything else. The parent writes
that byte only **after** cgroup limits are assigned. Skip this and the
child runs unconstrained during setup — no memory cap, no PID cap, no
I/O cap — for however long the parent spends doing netlink / veth /
proxy work. That's a DoS.

See [`run.rs`](../crates/agentjail/src/run.rs) `spawn()` and search for
`barrier_pipe`.

### 2. Seccomp is the last syscall you add in the child.

`apply_compiled(&bpf)` must be the final thing before `execve`. Any
syscall added after it that's in the deny list is dead on arrival.
Conversely, any syscall the **setup** needs that's in the deny list
has to run before we install the filter. If you add a new step to
`setup_child`, ask: does my syscall survive `SeccompLevel::Strict`?

The PID-namespace path ([`exec.rs`](../crates/agentjail/src/exec.rs)
`enter_pid_namespace_and_exec`) reapplies the filter inside the
grandchild — it's still "last before exec" there, just one layer down.

### 3. `pivot_root` + detach happens after mount setup, before `execve`.

All paths used during `setup_child` are host-absolute until
`mount::pivot_into` runs. After the pivot, the old root mount is
`umount2(MNT_DETACH)`-ed so there is no path walkable back to the host
FS — plain `chroot` is not used (it's escapable via nested-chroot +
`fchdir("..")` under `CAP_SYS_CHROOT`). After the pivot, `/workspace`,
`/output`, `/proc`, `/tmp` are jail-relative. If you add code that
reads a host path, it must run **before** `pivot_into`.

### 4. `/proc` is remounted inside the new PID namespace.

Without the fresh procfs mount (and its runc-style masking) in
[`exec.rs`](../crates/agentjail/src/exec.rs) `finish_and_exec`
(`mount::mount_proc` + `mount::mask_proc`), the jailed process sees the
host PID tree, or gets a writable `/proc/sys` / `/proc/sysrq-trigger`.
If either fails, we `_exit(127)` rather than continue.

### 5. PID namespace requires a double-fork.

`unshare(NEWPID)` does not move the current task into the new PID ns —
only its descendants. The intermediate process only waits and mirrors
the exit status; the grandchild (pid 1 of the new namespace) mounts
procfs, pivots, applies Landlock and seccomp, and `execve`s.

### 6. Netlink sockets bound before `unshare(NEWNET)` belong to the
   old netns.

Any netlink call in `setup_child` (add loopback, set address, add
default route) must open its socket **after** `enter_namespaces()`.
Our `netlink::*` free functions open-on-call, so this is enforced by
construction — don't cache a socket across the unshare line.

### 7. `PR_SET_PDEATHSIG(SIGKILL)` stays on.

The outer fork child sets it ([`run.rs`](../crates/agentjail/src/run.rs)),
and so does the grandchild inside the PID-namespace double-fork
([`exec.rs`](../crates/agentjail/src/exec.rs)). The kernel clears it on
every euid/egid change, so it is re-armed (with a `getppid` re-check)
after `setresuid(0)` in the user namespace and after the `run_as` drop.
Parent dies → jail dies → netns dies → veth cleanup happens. Without it
you leak `aj-h*` interfaces on the host until next boot.

The signal follows the *thread* that forked, not the process, so every
fork runs on one process-lifetime thread
([`spawner.rs`](../crates/agentjail/src/spawner.rs)); `Jail::spawn` is
safe from any thread. Tested by `jail_outlives_the_thread_that_spawned_it`
and `jail_dies_with_its_embedder`.

### 8. `JailHandle::Drop` must kill and reap if `reaped == false`.

If a caller drops the handle without calling `wait()` or
`wait_with_events()`, the process has to be killed and waited for, or
we leak a zombie + the cgroup can't be rmdir'd + the veth sticks
around. The `reaped` flag prevents killing a PID-recycled process on
the happy path.

### 9. `Drop` on `Cgroup` kills every pid in `cgroup.procs`.

A cgroup can't be removed while it has members. We SIGKILL any pid
still listed, then spin-poll `rmdir()` for up to 100 ms. If you add a
soft-stop path (SIGTERM with grace period), put it **above** the
current loop — the SIGKILL+rmdir is the last-resort janitor, not the
primary exit signal.

### 10. `wait()` drains stdout/stderr concurrently with the process
    wait.

`mem::replace` swaps the streams out of the handle into background
`tokio::spawn` tasks **before** `wait_for_pid` is awaited. The child
emits → pipes fill → drain tasks consume → child can keep writing. If
you "simplify" this to serial drain-after-wait, any child emitting
more than one pipe buffer (~64 KiB) will deadlock against its own
write. We've been here before.

### 11. Cgroup freezer waits for quiescence.

`fs::write(cgroup.freeze, "1")` only *requests* freeze. Tasks in
uninterruptible syscalls don't stop until those syscalls return.
`Cgroup::freeze()` and `freeze_cgroup()` both poll `cgroup.events`
until the kernel reports `frozen 1`, with a 50 ms deadline. If you
remove the poll, snapshots go back to being torn.

### 12. Symlinks are skipped, not followed.

Every directory walk in [`snapshot.rs`](../crates/agentjail/src/snapshot.rs)
and [`fork.rs`](../crates/agentjail/src/fork.rs) checks `is_symlink()`
and continues. Following them would escape the jail's output scope
(e.g., `/output/link → /etc/shadow`) during capture or restore. When
adding a new walker, skip symlinks.

### 13. Manifest format is versioned.

`Manifest.version: u32` starts at `1`. New `ManifestEntry` fields must
be `Option` + `#[serde(default, skip_serializing_if = "Option::is_none")]`
so older manifests round-trip. If you make an **incompatible** change
(removing a field, changing a type), bump the version — readers should
refuse to load a newer major than they understand.

### 14. Output paths must be UTF-8 when I/O limits are set.

`set_io_limit` refuses a non-UTF-8 output path. Falling back to `/`
would clamp the whole root device.

### 15. `/etc` is explicitly allow-listed.

`setup_root` mounts a tmpfs over `/etc` inside the jail and bind-mounts
only `ld.so.cache`, `ld.so.conf`, `resolv.conf`, `nsswitch.conf`,
`passwd`, `group`, `ssl`, `alternatives`. Never the full host `/etc`.
If you add a file, justify it: it must not leak host secrets (`shadow`,
`machine-id`, ssh keys).

### 16. `/tmp` is tmpfs with `NOEXEC`.

`mount_tmpfs_noexec(/tmp, 100 MiB)`. Prevents the
write-then-execute-from-tmp bypass when the jail's workspace is
read-only. If you swap `/tmp` to a persistent bind mount, the
`NOEXEC` has to come with it.

## The hot path, syscall by syscall

Rough cost breakdown for a `noop` jail:

1. Two pipes + barrier pipe: ~30 µs (3× `pipe2`).
2. `fork()`: ~200–300 µs, dominated by page-table copy — scales with
   parent RSS, so a slim launcher process is the main mitigation.
3. Child `unshare(NEWUSER|NEWNS|NEWNET|NEWIPC)`: ~50–100 µs.
4. Child mount loop (rootfs setup): ~300–400 µs, 15–20 `mount`
   syscalls. Each `/etc` file is a separate bind mount; so is each
   `/dev` node.
5. Child `pivot_root` + `umount2(MNT_DETACH)` + `execve`: ~60 µs.
6. Parent `pidfd_open` + `AsyncFd::readable().await`: ~50 µs from exit
   edge to wakeup.

`noop` p50 is roughly 1 ms on a recent x86-64 host; measure your own with
[`agentjail-bench`](../crates/agentjail-bench/).

## Adding a feature

- **New field in `JailConfig`**: has a default (keep struct update
  syntax working); wire it through `Jail::new` *before* fork if
  anything depends on it (like seccomp level → BPF compile).
- **New syscall in `setup_child`**: check it survives `SeccompLevel::
  Standard` *and* `Strict`. If not, explicitly allow it in
  [`seccomp.rs`](../crates/agentjail/src/seccomp.rs) — and write a
  test.
- **New bind mount in `setup_root`**: justify it against #15. Add a
  test that reads the mount from inside a jail.
- **New cgroup controller**: add it to the `+memory +cpu +pids +io`
  write in [`cgroup.rs`](../crates/agentjail/src/cgroup.rs). Kernel
  accepts space-separated enable lists in one write.
- **New seccomp block**: add the syscall to `base_blocked_syscalls()`
  and add a regression test in [`tests/audit_regression_test.rs`](../crates/agentjail/tests/audit_regression_test.rs).
- **New netlink op**: open the socket on call (don't cache across
  `unshare(NEWNET)`). Use monotonic `nlmsg_seq` — kernel echoes it on
  ACK.

## Testing

Integration tests need root, and each test binary needs a cgroup of its
own (the leaf rule): run them through
[`cgroup_leaf_runner.sh`](../crates/agentjail/tests/support/cgroup_leaf_runner.sh)
as the CI job does, or with `make test-rust-privileged`.

- `tests/integration.rs` — happy-path round-trips.
- `tests/audit_regression_test.rs` — most rows in the threat-model
  table in [SECURITY.md](../SECURITY.md) have a test here. Don't delete
  one without deleting the row.
- `tests/security_test.rs` — escape attempts. New attack surface gets
  a test here.
- `tests/malicious_test.rs` — adversarial input patterns.
- `tests/fork_test.rs` — `live_fork` (COW correctness, concurrent
  forks, chain).
- `tests/snapshot_test.rs` — full + incremental + frozen, and the
  symlink-swap race.
- `tests/pdeathsig_test.rs`, `tests/jail_root_test.rs`,
  `tests/firewall_test.rs` — lifecycle checks that need their own
  binary: a SIGKILLed embedder takes its jails with it, spawns leave no
  jail root behind, a removed nftables table comes back.
- `tests/inbound_reach_test.rs` — host→jail port forwarding via veth.
- `tests/seccomp_blocklist_test.rs` — rebuilds the filter and asserts
  each blocked syscall is actually denied.

## Benchmarks

[`crates/agentjail-bench`](../crates/agentjail-bench/) measures
spawn-to-exit latency, snapshot and restore throughput, and `live_fork`
time. Results are host-specific and not committed; compare two `--json`
runs of the same scenario before and after a change.

```
cargo run -p agentjail-bench --release -- <scenario> [--concurrency N] [--iters N] [--json PATH]
```

Scenarios: `noop`, `noop-full`, `stdout-heavy`, `snapshot-create`,
`snapshot-restore`, `snapshot-repeat`, `live-fork` (`--help` lists the
options). A new scenario is a file under `src/scenarios/` registered in
`scenarios::dispatch`.

## Pitfalls to avoid

- **Adding `.await` between `fork()` and `exec()`**: you're in a
  single-threaded, post-fork context. Tokio tasks, allocators, locks —
  all unsound. Only `libc::*` and `rustix::*` are safe in the child
  branch before `execve`. See [`run.rs`](../crates/agentjail/src/run.rs)
  for the allowed vocabulary.
- **Caching fds across `unshare(NEWNET)`**: they bind to the old netns.
  This is why all netlink ops open-on-call.
- **Using `std::fs::File` for async pipe reads**: it'll compile but
  routes every read through tokio's blocking pool. Use
  `tokio::net::unix::pipe::Receiver` for pipes and `AsyncFd` for
  everything else.
- **`#[serde(deny_unknown_fields)]` on `Manifest`**: will lock out
  forward-compat. Leave it off.
