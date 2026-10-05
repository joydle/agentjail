# bench-results/

Committed JSON artifacts from [`agentjail-bench`](../crates/agentjail-bench/).
Reference set only — git log is the history.

## Naming

`{scenario}-{state}.json` where `state` is one of:

- `baseline` — pre-optimization reference. Frozen; only refreshed when we
  consciously adopt a new baseline.
- `current` — the committed state as of `HEAD`. Updated in place when an
  optimization lands; the previous `current` goes into git history via
  the commit diff.

No per-optimization files, no intermediate milestones. If a PR changes
perf behaviour, it replaces `{scenario}-current.json` and documents the
delta in [`../BENCH_RESULTS.md`](../BENCH_RESULTS.md).

Each JSON embeds its own `env` block (kernel, CPU count, cgroup-v2
status, userns availability, `agentjail_rev`) so it's self-describing
regardless of when it was captured.

## Runs

All captured inside Docker (`docker compose run --rm dev …`) so kernel
+ toolchain are pinned across runs. See each JSON's `env.agentjail_rev`
for the exact git SHA the numbers were measured against.

## Reproducing

See [`../BENCH_RESULTS.md`](../BENCH_RESULTS.md) for the command block.
Full harness spec in [`../crates/agentjail-bench/README.md`](../crates/agentjail-bench/README.md).
