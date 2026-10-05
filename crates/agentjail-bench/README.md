# agentjail-bench

Scenario-driven benchmark harness for the `agentjail` crate. Measures
spawn-to-exit latency, snapshot/restore throughput, and live-fork wall
time under configurable concurrency.

Every perf change lands with a before/after pair of JSON runs committed
to [`bench-results/`](../../bench-results/). The summary table lives in
[`BENCH_RESULTS.md`](../../BENCH_RESULTS.md).

## Requirements

- Linux. The harness links `agentjail`, which is Linux-only (namespaces,
  cgroups v2, seccomp, landlock).
- For realistic numbers: a rootful host or a user with `userns` + `cgroupv2`
  delegation. Scenarios that need the cgroup freezer (`live-fork`) will
  skip themselves if `cgroup.freeze` isn't writable.
- For the Allowlist scenarios: `CAP_NET_ADMIN` (root or unprivileged
  userns with veth delegation).

## Usage

```
cargo run -p agentjail-bench --release -- <scenario> [options]

OPTIONS
  --concurrency N    In-flight jails (default: 1)
  --iters N          Total iterations measured (default: 200)
  --warmup N         Warmup iterations, not recorded (default: 10)
  --json PATH        Write full result JSON here
  --tree-files N     File count for snapshot scenarios (default: 1000)
  --tree-size-kb K   Per-file size for snapshot scenarios (default: 4)
  -h, --help         Print help

SCENARIOS
  noop               /bin/true, seccomp disabled, no network
  noop-full          /bin/true with SeccompLevel::Standard + user ns
  stdout-heavy       Jail writes 10 MiB to stdout, parent collects
  snapshot-create    Full Snapshot::create over a fabricated tree
  snapshot-restore   Restore a previously captured snapshot
  snapshot-repeat    Second incremental snapshot (should be near-zero I/O
                     after 1.5 fast-path lands)
  live-fork          live_fork of a running jail; records clone_method
```

## Output

Prints a compact summary to stdout:

```
scenario=noop  concurrency=10  iters=200  errors=0
  wall=4.23s  throughput=47.3/s
  latency_us  min=2100  p50=2800  p95=4500  p99=9200  max=15000
```

With `--json PATH`, also writes a full result document:

```json
{
  "scenario": "noop",
  "concurrency": 10,
  "iters": 200,
  "warmup": 10,
  "errors": 0,
  "wall_clock_s": 4.23,
  "throughput_per_s": 47.3,
  "latency_us": {
    "min": 2100, "p50": 2800, "p95": 4500,
    "p99": 9200, "max": 15000,
    "mean": 3150, "stddev": 890
  },
  "extra": { "clone_method": "reflink" },
  "env": {
    "kernel": "6.8.0-...",
    "cpu_count": 16,
    "user_namespace": true,
    "cgroup_v2": true,
    "agentjail_rev": "23bcc18"
  },
  "timestamp_unix": 1713825200
}
```

## Comparing runs

Commit JSON output under `bench-results/<scenario>-<branch>.json`, then:

```
jq -s '.[0].latency_us.p50 as $before
     | .[1].latency_us.p50 as $after
     | { before: $before, after: $after, ratio: ($after / $before) }' \
   before.json after.json
```

A CI job should fail on regressions beyond an agreed threshold (say 20%
on p99). Not wired yet.

## What's measured, what isn't

- **Measured:** wall-clock latency of the user-visible operation
  (`jail.run(...)`, `Snapshot::create(...)`, `jail.live_fork(...)`).
- **Not measured (yet):** memory per jail, kernel-side syscall cost
  breakdown, tail-latency under sustained load for hours. Those want
  `perf`/`bpftrace`/`rss`-tracking wrappers — out of scope for phase 0.

## Adding a scenario

1. Add a file under `src/scenarios/`.
2. Implement `pub async fn run(cfg: &ScenarioConfig) -> Result<Iteration>`.
3. Register in `scenarios::dispatch`.
4. Document it in this README.

See `src/scenarios/noop.rs` for the minimal shape.
