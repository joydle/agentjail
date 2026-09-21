// Wire types. Mirror `crates/agentjail-ctl/src/routes/*` exactly; any
// drift breaks the SDK against a deployed server.

export type ServiceId = "openai" | "anthropic" | "github" | "stripe";

export interface CredentialRecord {
  service: ServiceId;
  added_at: string;
  updated_at: string;
  /** Non-reversible fingerprint of the secret (for rotation detection). */
  fingerprint: string;
}

export interface Session {
  id: string;
  created_at: string;
  expires_at: string | null;
  services: ServiceId[];
  /** Phantom tokens + matching *_BASE_URL entries pointing at the proxy. */
  env: Record<string, string>;
}

export interface AuditRow {
  id: number;
  at: string;
  session_id: string;
  service: string;
  method: string;
  path: string;
  status: number;
  reject_reason: string | null;
  upstream_ms: number | null;
}

export interface AuditList {
  rows: AuditRow[];
  total: number;
}

export interface ExecResult {
  stdout: string;
  stderr: string;
  exit_code: number;
  duration_ms: number;
  timed_out: boolean;
  oom_killed: boolean;
  stats?: ResourceStats;
}

export interface ResourceStats {
  memory_peak_bytes: number;
  memory_current_bytes: number;
  cpu_usage_usec: number;
  io_read_bytes: number;
  io_write_bytes: number;
  pids_current: number;
}

/**
 * Network policy for a jail. Match the shapes on the wire:
 *   { mode: "none" }
 *   { mode: "loopback" }
 *   { mode: "allowlist", domains: string[] }
 */
export type NetworkSpec =
  | { mode: "none" }
  | { mode: "loopback" }
  | { mode: "allowlist"; domains: string[] };

/** Seccomp level exposed on the wire. `"disabled"` is intentionally omitted. */
export type SeccompSpec = "standard" | "strict";

/**
 * Optional jail tuning shared by `sessions.exec` and `runs.create`.
 *
 * - `cpuPercent` is clamped server-side to `1..=800` (800 = 8 cores).
 * - `maxPids` is clamped to `1..=1024`.
 * - `network` defaults to `{ mode: "none" }`; `allowlist` domains must be
 *   hostnames or trailing-`*` globs (no scheme).
 */
export interface ExecOptions {
  network?: NetworkSpec;
  seccomp?: SeccompSpec;
  cpuPercent?: number;
  maxPids?: number;
}

/** Parameters for a one-shot run. */
export interface RunRequest extends ExecOptions {
  code: string;
  language?: "javascript" | "python" | "bash";
  timeoutSecs?: number;
  memoryMb?: number;
}

/** One child of an N-way fork. */
export interface ForkChild {
  code: string;
  memoryMb?: number;
}

/**
 * Parameters for `aj.runs.fork`. Supports either a single child
 * (legacy `childCode` field) or N children (`children`, up to 16).
 * Server populates both legacy and new response fields so either shape
 * of request works.
 */
export interface ForkRequest extends ExecOptions {
  parentCode: string;
  /** Legacy single-child shorthand. */
  childCode?: string;
  /** N-way children (up to 16). Mutually exclusive with `childCode`. */
  children?: ForkChild[];
  language?: "javascript" | "python" | "bash";
  /** How long the parent runs before we freeze + fork. Default 1500ms. */
  forkAfterMs?: number;
  timeoutSecs?: number;
  memoryMb?: number;
}

/** Metadata for a completed live_fork. */
export interface ForkMeta {
  clone_ms: number;
  files_cloned: number;
  files_cow: number;
  bytes_cloned: number;
  method: string;
  was_frozen: boolean;
}

/**
 * Response from `aj.runs.fork`. Both legacy single-child fields
 * (`child`, `fork`) and N-way arrays (`children`, `forks`) are populated
 * so existing callers keep working. For N-way calls, `child === children[0]`
 * and `fork === forks[0]`.
 */
export interface ForkResult {
  parent: ExecResult;
  /** First child (back-compat). Equals `children[0]`. */
  child: ExecResult;
  /** All children in invocation order. */
  children: ExecResult[];
  /** ForkMeta for the first child (back-compat). Equals `forks[0]`. */
  fork: ForkMeta;
  /** Per-child ForkMeta. */
  forks: ForkMeta[];
}

/** Kinds of jail run — one per exec endpoint. */
export type JailKind = "run" | "exec" | "fork" | "stream" | "workspace";

/** Jail record lifecycle. */
export type JailStatus = "running" | "completed" | "error";

/**
 * Snapshot of the jail config that the invocation ran with. Captured
 * at start time so the Jails ledger can answer "what did this run
 * with?" — safe-to-display fields only, no secrets.
 */
export interface JailConfigSnapshot {
  network_mode:     "none" | "loopback" | "allowlist";
  network_domains?: string[];
  seccomp:          "standard" | "strict";
  memory_mb:        number;
  timeout_secs:     number;
  cpu_percent:      number;
  max_pids:         number;
  git_repo?:        string | null;
  git_ref?:         string | null;
}

/** A single row in `GET /v1/jails`. */
export interface JailRecord {
  id: number;
  kind: JailKind;
  started_at: string;        // RFC3339
  ended_at: string | null;
  status: JailStatus;
  session_id: string | null;
  label: string;

  exit_code:   number | null;
  duration_ms: number | null;
  timed_out:   boolean | null;
  oom_killed:  boolean | null;
  memory_peak_bytes:    number | null;
  memory_current_bytes: number | null;
  cpu_usage_usec:       number | null;
  io_read_bytes:        number | null;
  io_write_bytes:       number | null;
  pids_current:         number | null;
  stdout: string | null;
  stderr: string | null;
  error:  string | null;

  /**
   * What the jail actually ran with. `undefined` for rows predating
   * this feature; populated on every new run.
   */
  config?: JailConfigSnapshot;
}

export interface JailsList {
  rows:  JailRecord[];
  total: number;
}

/**
 * One event from `aj.runs.stream`. Maps to the server's SSE `event:` frames.
 * `completed` is always the last event before the stream closes.
 */
export type StreamEvent =
  | { type: "started";   pid: number }
  | { type: "stdout";    line: string }
  | { type: "stderr";    line: string }
  | { type: "stats";
      memory_peak_bytes: number;
      memory_current_bytes: number;
      cpu_usage_usec: number;
      io_read_bytes: number;
      io_write_bytes: number;
      pids_current: number;
    }
  | { type: "completed";
      exit_code: number;
      duration_ms: number;
      timed_out: boolean;
      oom_killed: boolean;
      memory_peak_bytes: number;
      memory_current_bytes: number;
      cpu_usage_usec: number;
      pids_current: number;
    }
  | { type: "error";     message: string };

// ---------------- workspaces ----------------

/**
 * Persisted jail config shipped with a workspace. Re-applied on every
 * exec so the user's network/seccomp/limits decisions survive restarts.
 */
export interface WorkspaceSpec {
  memory_mb: number;
  timeout_secs: number;
  cpu_percent: number;
  max_pids: number;
  network_mode: "none" | "loopback" | "allowlist";
  network_domains: string[];
  seccomp: SeccompSpec;
  /** Auto-pause after N seconds of inactivity. 0 = never. */
  idle_timeout_secs: number;
}

/**
 * One (hostname, backend) forwarding entry served by the gateway
 * listener. `backend_url` is caller-supplied — the gateway does not
 * discover jail-internal IPs.
 */
/**
 * One hostname-routed entry on a workspace. Exactly one of
 * `backend_url` / `vm_port` must be set; the gateway either forwards
 * to the static URL or resolves `vm_port` against the workspace's
 * live jail IP at request time.
 */
export interface WorkspaceDomain {
  /** Hostname the gateway matches against the `Host` header. */
  domain: string;
  /** Where to forward matched requests, e.g. `http://10.0.0.5:3000`.
   *  Mutually exclusive with `vm_port`. */
  backend_url?: string;
  /** Port bound *inside* the workspace's jail; resolved to
   *  `http://<live_jail_ip>:<vm_port>/` when an exec is in flight. */
  vm_port?: number;
}

/** A persistent workspace (long-lived mount tree). */
export interface Workspace {
  id: string;
  /** RFC3339. */
  created_at: string;
  /** RFC3339 or null. */
  deleted_at: string | null;
  source_dir: string;
  output_dir: string;
  config: WorkspaceSpec;
  git_repo: string | null;
  git_ref: string | null;
  label: string | null;
  /** Hostname → backend-URL forwards served by the gateway listener. */
  domains: WorkspaceDomain[];
  /** RFC3339 or null. */
  last_exec_at: string | null;
  /**
   * RFC3339 or null. When set, the idle-reaper paused this workspace
   * and captured `auto_snapshot`; the next exec auto-restores.
   */
  paused_at: string | null;
  /** Snapshot id captured at pause time. Cleared on resume. */
  auto_snapshot: string | null;
}

/** Paginated list response from `GET /v1/workspaces`. */
export interface WorkspaceList {
  rows: Workspace[];
  total: number;
  limit: number;
  offset: number;
}

/**
 * Parameters accepted by `aj.workspaces.create`. Mirrors the jail-config
 * knobs of a one-shot run, plus an optional git clone into `source_dir`.
 */
/**
 * Git seed for a new workspace. Use the single-repo form for one
 * checkout, or `repos` for a multi-repo agent dev environment.
 */
export type GitSeed =
  | { repo: string; ref?: string }
  | { repos: { repo: string; ref?: string; dir?: string }[] };

export interface WorkspaceCreateRequest extends ExecOptions {
  git?: GitSeed;
  label?: string;
  memoryMb?: number;
  timeoutSecs?: number;
  /** Auto-pause after this many seconds of inactivity. 0 = never. */
  idleTimeoutSecs?: number;
  /** Inbound hostname forwards served by the server's gateway listener. */
  domains?: WorkspaceDomain[];
  /**
   * Runtime flavors mounted read-only into the jail (e.g.
   * `["nodejs", "python"]`). Each name is resolved against
   * `GET /v1/flavors`; unknown names 400 at create.
   */
  flavors?: string[];
}

/**
 * Response from `workspaces.fork`. `forks.length === count`; each entry
 * is a fully independent workspace. `snapshot_id` is the checkpoint
 * captured on the parent — kept around so you can clone more copies
 * later via `snapshots.createWorkspaceFrom`.
 */
export interface WorkspaceForkResponse {
  parent: Workspace;
  forks: Workspace[];
  snapshot_id: string;
}

/** One exec against a workspace. */
export interface WorkspaceExecRequest {
  cmd: string;
  args?: string[];
  timeoutSecs?: number;
  memoryMb?: number;
  /** Additional env pairs appended to the session base (PATH always set). */
  env?: [string, string][];
}

// ---------------- snapshots ----------------

/** One snapshot row. */
export interface SnapshotRecord {
  id: string;
  workspace_id: string | null;
  name: string | null;
  /** RFC3339. */
  created_at: string;
  path: string;
  size_bytes: number;
}

/** Paginated list from `GET /v1/snapshots`. */
export interface SnapshotList {
  rows: SnapshotRecord[];
  total: number;
  limit: number;
  offset: number;
}

/** One file inside a pool-backed snapshot. */
export interface SnapshotManifestEntry {
  path:   string;
  mode:   number;
  /** Hex-encoded BLAKE3-256 of the file bytes. */
  hash: string;
  size:   number;
}

/**
 * Listing returned by `GET /v1/snapshots/:id/manifest`. `kind` is
 * `"incremental"` for pool-backed snapshots (entries populated) or
 * `"classic"` for full-copy snapshots where the file list isn't
 * persisted (entries empty).
 */
export interface SnapshotManifest {
  kind:    "incremental" | "classic";
  entries: SnapshotManifestEntry[];
}

// ---------------- public (no-auth) ----------------

/** Live counters returned by `GET /v1/stats`. */
export interface PublicStats {
  active_execs: number;
  total_execs: number;
  sessions: number;
  credentials: number;
}

// ---------------- settings ----------------

/** One phantom-proxy upstream. */
export interface ProviderInfo {
  service_id:     string;
  upstream_base:  string;
  request_prefix: string;
}

/** Read-only snapshot returned by `GET /v1/config`. */
export interface SettingsSnapshot {
  proxy: {
    base_url:  string;
    bind_addr: string | null;
    providers: ProviderInfo[];
  };
  control_plane: {
    bind_addr: string | null;
  };
  gateway: {
    bind_addr: string;
  } | null;
  exec: {
    default_memory_mb:    number;
    default_timeout_secs: number;
    max_concurrent:       number;
  } | null;
  persistence: {
    state_dir:         string;
    snapshot_pool_dir: string | null;
    idle_check_secs:   number;
  };
  snapshots: {
    gc: {
      max_age_secs: number | null;
      max_count:    number | null;
      tick_secs:    number;
    } | null;
  };
}
