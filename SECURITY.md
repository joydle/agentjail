# Security

Report vulnerabilities privately through
[GitHub Security Advisories](https://github.com/joydle/agentjail/security/advisories/new),
not public issues. Include the affected crate, the commit, the kernel
version and config, steps to reproduce, and the impact. Expect an
acknowledgement within 72 hours.

agentjail is pre-1.0; only `main` receives fixes.

## Threat model

The jailed process is the attacker. In scope: anything that lets it read
or write host files outside `/workspace` and `/output`, see or signal host
processes, reach the network beyond its `Network` mode, exceed its cgroup
limits, keep running past its timeout or its embedder, or exhaust the
embedding process (proxy, output pipes, snapshots). For the control
plane, also: crossing tenants, reading a real upstream credential, or
acting without a valid `AGENTJAIL_API_KEY` entry.

Out of scope: kernel vulnerabilities (agentjail is not a VM), GPU-mode
exposure of the NVIDIA driver, and a control plane started with
`AGENTJAIL_DEV_INSECURE=1` (no API key; every caller is admin by
design). Known limitations are listed under
[Limits](README.md#limits) in the README.

Each row names the regression test that fails if the protection breaks.

| Attack | Protection | Test |
|---|---|---|
| Read host `~/.ssh` / `~/.aws` | Not mounted | [`test_cannot_read_ssh_keys`](crates/agentjail/tests/security_test.rs) |
| Read `/etc/shadow`, machine-id | Minimal `/etc` | [`test_etc_shadow_not_accessible`](crates/agentjail/tests/audit_regression_test.rs) |
| Network exfiltration | Netns + allowlist proxy | [`test_network_none_blocks_external`](crates/agentjail/tests/security_test.rs), [`test_reverse_shell_blocked`](crates/agentjail/tests/security_test.rs) |
| Fork bomb | PID limit | [`test_pid_limit_blocks_fork_bomb`](crates/agentjail/tests/audit_regression_test.rs) |
| Memory blow-up | Memory limit + OOM detection | — |
| Disk thrashing | I/O bandwidth limits | [`test_io_write_bandwidth_limit_enforced`](crates/agentjail/tests/audit_regression_test.rs) |
| Signal host processes | PID namespace | [`test_pid_namespace_full_sandbox`](crates/agentjail/tests/security_test.rs) |
| Mount manipulation | `mount` + new mount API blocked | [`seccomp_standard_blocks_documented_syscalls`](crates/agentjail/tests/seccomp_blocklist_test.rs) |
| `chroot` escape | `pivot_root` + detach; `chroot` seccomp-blocked | [`test_chroot_no_home`](crates/agentjail/tests/security_test.rs) |
| io_uring bypass | `io_uring_*` blocked | [`seccomp_standard_blocks_documented_syscalls`](crates/agentjail/tests/seccomp_blocklist_test.rs) |
| Compat-mode escape | `personality()` blocked | [`seccomp_standard_blocks_documented_syscalls`](crates/agentjail/tests/seccomp_blocklist_test.rs) |
| Namespace escape | `unshare`, `setns`, `clone` with `CLONE_NEW*` blocked; `clone3` → `ENOSYS` | [`test_seccomp_blocks_unshare`](crates/agentjail/tests/audit_regression_test.rs), [`clone_namespace_flags_blocked_but_fork_threads_spawn_work`](crates/agentjail/tests/core_isolation_test.rs) |
| x32 / foreign-arch syscalls | Killed by the filter prelude | [`x86_64_full_filter_kills_x32`](crates/agentjail/src/seccomp.rs) (x86_64 hosts only) |
| Jail runs as host root | userns maps root to an unprivileged id; else `run_as` | [`jail_never_runs_as_host_root`](crates/agentjail/tests/core_isolation_test.rs) |
| Host writes via `/proc` (`core_pattern`, sysrq) | Masked `/proc` | [`proc_sys_and_sysrq_are_read_only_for_root_embedder`](crates/agentjail/tests/core_isolation_test.rs) |
| Reach host services / other jails | nftables table + proxy peer pinning | [`jail_cannot_reach_other_jail_proxy_or_host_listener`](crates/agentjail/tests/net_isolation_test.rs) |
| Read outside the jail's paths | Landlock ruleset | [`landlock_preset_allows_toolchain_and_denies_the_rest`](crates/agentjail/tests/core_isolation_test.rs) |
| BPF / perf | `bpf`, `perf_event_open`, `userfaultfd` blocked | [`test_seccomp_blocks_bpf`](crates/agentjail/tests/audit_regression_test.rs) |
| Executable memory | `memfd_create` blocked | [`seccomp_standard_blocks_documented_syscalls`](crates/agentjail/tests/seccomp_blocklist_test.rs) |
| Write + exec on `/tmp` | `NOEXEC` | [`test_tmp_noexec`](crates/agentjail/tests/audit_regression_test.rs) |
| Setuid escalation | `PR_SET_NO_NEW_PRIVS` | — |
| Core-dump leak | `RLIMIT_CORE=0` | [`test_rlimit_core_disabled`](crates/agentjail/tests/audit_regression_test.rs) |
| Parent stdout OOM | Output capped at 256 MiB / stream | [`test_large_stdout_does_not_oom`](crates/agentjail/tests/audit_regression_test.rs) |
| FD exhaustion | `RLIMIT_NOFILE` at 4096 | [`test_fd_limit_enforced`](crates/agentjail/tests/audit_regression_test.rs) |
| Symlink traversal | Skipped in snapshots, forks, cleanup | [`test_snapshot_restore_does_not_follow_symlinks`](crates/agentjail/tests/audit_regression_test.rs) |
| Swap a directory for a symlink during a snapshot | Walk by directory fd, `O_NOFOLLOW` at every level; no hot copy of an unfrozen jail | [`snapshot_walk_never_follows_a_swapped_directory`](crates/agentjail/tests/snapshot_test.rs) |
| Zombie / fd leak | Drop kills and reaps | [`test_no_zombie_after_drop`](crates/agentjail/tests/audit_regression_test.rs) |
| Outlive the embedder | `PR_SET_PDEATHSIG`, re-armed after each credential change; forks on a process-lifetime thread | [`jail_dies_with_its_embedder`](crates/agentjail/tests/pdeathsig_test.rs), [`jail_outlives_the_thread_that_spawned_it`](crates/agentjail/tests/core_isolation_test.rs) |
| Cross-tenant read | `tenant_id` on every row; list filters, get 404s | [`operator_cannot_read_other_tenants_workspace_by_id`](crates/agentjail-ctl/tests/api.rs), [`credentials_are_tenant_scoped`](crates/agentjail-ctl/tests/api.rs) |
| Token spent on another tenant's bill | `TokenRecord.tenant_id`; proxy looks up `keys.get(tenant, service)` | [`agentjail-phantom`](crates/agentjail-phantom/src/proxy.rs) |
| Malicious `.gitmodules` / `core.sshCommand` | Clone-jail (workspace clones): Standard seccomp, allowlist net, no host access | [`clone_jail_clones_a_small_public_repo`](crates/agentjail-ctl/tests/clone_jail_test.rs) |
| Operator reads platform internals via `GET /v1/config` | Admin-only fields omitted for operators | [`settings_bind_addrs_hidden_from_operators`](crates/agentjail-ctl/tests/api.rs) |
| Snapshot rehydrate spoofing | Requires `parent_workspace_id`, verified | [`from_snapshot_requires_and_checks_parent_workspace_id`](crates/agentjail-ctl/tests/api.rs) |
