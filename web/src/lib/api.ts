import type { ServiceId } from "./format";

export type { ServiceId };

export interface CredentialRecord {
  /** Tenant that owns the credential. Stamped server-side. */
  tenant_id: string;
  service: ServiceId;
  added_at: string;
  updated_at: string;
  fingerprint: string;
}

export interface Session {
  id: string;
  created_at: string;
  expires_at: string | null;
  services: ServiceId[];
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

export interface Stats {
  active_execs: number;
  total_execs: number;
  sessions: number;
  credentials: number;
}

export interface ProviderInfo {
  service_id:     string;
  upstream_base:  string;
  request_prefix: string;
}

export interface SettingsSnapshot {
  proxy: {
    base_url:  string;
    /** Omitted for non-admin scopes — host bind-addr is operator-only. */
    bind_addr?: string;
    providers: ProviderInfo[];
  };
  control_plane: { bind_addr?: string };
  /** Admin-only. Absent for operator role. */
  gateway?: { bind_addr: string } | null;
  exec: {
    default_memory_mb:    number;
    default_timeout_secs: number;
    max_concurrent:       number;
  } | null;
  persistence: {
    /** Admin-only. Absent for operator role. */
    state_dir?:        string;
    snapshot_pool_dir?: string;
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

export type JailKind   = "run" | "exec" | "fork" | "stream" | "workspace";
export type JailStatus = "running" | "completed" | "error";

export interface WorkspaceSpec {
  memory_mb: number;
  timeout_secs: number;
  cpu_percent: number;
  max_pids: number;
  network_mode: "none" | "loopback" | "allowlist";
  network_domains: string[];
  seccomp: "standard" | "strict";
  idle_timeout_secs: number;
  /**
   * Names of runtime flavors bind-mounted into the jail (see /v1/flavors).
   * Empty for workspaces created before flavors existed.
   */
  flavors?: string[];
}

export interface WorkspaceDomain {
  domain: string;
  /** Mutually exclusive with `vm_port`. */
  backend_url?: string;
  /** Jail-internal port; resolved to live jail IP at request time. */
  vm_port?: number;
}

export interface Workspace {
  id: string;
  created_at: string;
  deleted_at: string | null;
  source_dir: string;
  output_dir: string;
  config: WorkspaceSpec;
  git_repo: string | null;
  git_ref: string | null;
  label: string | null;
  domains: WorkspaceDomain[];
  last_exec_at: string | null;
  paused_at: string | null;
  auto_snapshot: string | null;
}

export interface WorkspaceList {
  rows: Workspace[];
  total: number;
  limit: number;
  offset: number;
}

export interface WorkspaceCreateRequest {
  git?: { repo: string; ref?: string };
  label?: string;
  memory_mb?: number;
  timeout_secs?: number;
  idle_timeout_secs?: number;
  network?: { mode: "none" } | { mode: "loopback" } | { mode: "allowlist"; domains: string[] };
  seccomp?: "standard" | "strict";
  cpu_percent?: number;
  max_pids?: number;
  /**
   * Runtime flavors to mount into the jail. Each name is validated
   * server-side against `GET /v1/flavors`; unknown names fail with a 400.
   */
  flavors?: string[];
}

export interface SnapshotRecord {
  id: string;
  workspace_id: string | null;
  name: string | null;
  created_at: string;
  path: string;
  size_bytes: number;
}

export interface SnapshotList {
  rows: SnapshotRecord[];
  total: number;
  limit: number;
  offset: number;
}

export interface SnapshotManifestEntry {
  path: string;
  mode: number;
  /** Hex-encoded BLAKE3-256 of the file bytes. */
  hash: string;
  size: number;
}

export interface SnapshotManifest {
  kind:    "incremental" | "classic";
  entries: SnapshotManifestEntry[];
}

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

export interface JailRecord {
  id: number;
  kind: JailKind;
  started_at: string;
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
  parent_id: number | null;
  /** Captured at start time; undefined on rows predating this feature. */
  config?: JailConfigSnapshot;
}

export interface JailsList {
  rows:  JailRecord[];
  total: number;
  limit: number;
  offset: number;
}

export interface JailsQuery {
  limit?:  number;
  offset?: number;
  status?: JailStatus;
  kind?:   JailKind;
  q?:      string;
}

export interface ExecResult {
  stdout: string;
  stderr: string;
  exit_code: number;
  duration_ms: number;
  timed_out: boolean;
  oom_killed: boolean;
  stats?: {
    memory_peak_bytes:    number;
    memory_current_bytes: number;
    cpu_usage_usec:       number;
    io_read_bytes:        number;
    io_write_bytes:       number;
    pids_current:         number;
  };
}

export class ApiError extends Error {
  readonly status: number;
  constructor(status: number, message: string) {
    super(message);
    this.name = "ApiError";
    this.status = status;
  }
}

export function createApi(baseUrl: string, apiKey: string) {
  async function call<T>(method: string, path: string, body?: unknown): Promise<T> {
    const headers: Record<string, string> = { accept: "application/json" };
    if (apiKey) headers.authorization = `Bearer ${apiKey}`;
    if (body !== undefined) headers["content-type"] = "application/json";

    const res = await fetch(`${baseUrl}${path}`, {
      method,
      headers,
      body: body !== undefined ? JSON.stringify(body) : undefined,
    });

    if (res.status === 204) return undefined as T;
    const text = await res.text();
    const parsed = text ? safeParse(text) : null;
    if (!res.ok) {
      const msg =
        parsed && typeof parsed === "object" && "error" in parsed
          ? String((parsed as { error: unknown }).error)
          : res.statusText || `HTTP ${res.status}`;
      throw new ApiError(res.status, msg);
    }
    return (parsed ?? text) as T;
  }

  function safeParse(text: string): unknown {
    try {
      return JSON.parse(text);
    } catch {
      return null;
    }
  }

  return {
    health: () => call<string>("GET", "/healthz"),
    stats: () => call<Stats>("GET", "/v1/stats"),

    credentials: {
      // `tenant` is only meaningful for admin callers browsing a
      // different tenant's creds; operators can pass their own tenant
      // or omit it (the server scopes either way).
      list: (tenant?: string) =>
        call<CredentialRecord[]>(
          "GET",
          `/v1/credentials${tenant ? `?tenant=${encodeURIComponent(tenant)}` : ""}`,
        ),
      put: (service: ServiceId, secret: string, tenant?: string) =>
        call<CredentialRecord>(
          "POST",
          `/v1/credentials${tenant ? `?tenant=${encodeURIComponent(tenant)}` : ""}`,
          { service, secret },
        ),
      delete: (service: ServiceId, tenant?: string) =>
        call<void>(
          "DELETE",
          `/v1/credentials/${service}${tenant ? `?tenant=${encodeURIComponent(tenant)}` : ""}`,
        ),
    },

    sessions: {
      list: () => call<Session[]>("GET", "/v1/sessions"),
      create: (services: ServiceId[], ttlSecs?: number) =>
        call<Session>("POST", "/v1/sessions", {
          services,
          ...(ttlSecs !== undefined ? { ttl_secs: ttlSecs } : {}),
        }),
      close: (id: string) => call<void>("DELETE", `/v1/sessions/${id}`),
    },

    runs: {
      create: (code: string, language = "javascript", timeoutSecs?: number) =>
        call<ExecResult>("POST", "/v1/runs", {
          code,
          language,
          ...(timeoutSecs !== undefined ? { timeout_secs: timeoutSecs } : {}),
        }),
    },

    audit: {
      recent: (limit = 100) =>
        call<AuditList>("GET", `/v1/audit?limit=${limit}`),
    },

    jails: {
      list: (params?: JailsQuery) => {
        const q = new URLSearchParams();
        if (params?.limit  != null) q.set("limit",  String(params.limit));
        if (params?.offset != null) q.set("offset", String(params.offset));
        if (params?.status)         q.set("status", params.status);
        if (params?.kind)           q.set("kind",   params.kind);
        if (params?.q)              q.set("q",      params.q);
        const qs = q.toString();
        return call<JailsList>("GET", `/v1/jails${qs ? `?${qs}` : ""}`);
      },
      get: (id: number) => call<JailRecord>("GET", `/v1/jails/${id}`),
    },

    workspaces: {
      list: (params?: { limit?: number; offset?: number; q?: string }) => {
        const qs0 = new URLSearchParams();
        if (params?.limit != null)  qs0.set("limit",  String(params.limit));
        if (params?.offset != null) qs0.set("offset", String(params.offset));
        if (params?.q)              qs0.set("q",      params.q);
        const qs = qs0.toString();
        return call<WorkspaceList>("GET", `/v1/workspaces${qs ? `?${qs}` : ""}`);
      },
      get: (id: string) => call<Workspace>("GET", `/v1/workspaces/${id}`),
      create: (req: WorkspaceCreateRequest) =>
        call<Workspace>("POST", "/v1/workspaces", req),
      rename: (id: string, label: string) =>
        call<Workspace>("PATCH", `/v1/workspaces/${id}`, { label }),
      delete: (id: string) => call<void>("DELETE", `/v1/workspaces/${id}`),
      exec: (
        id: string,
        req: { cmd: string; args?: string[]; timeout_secs?: number; memory_mb?: number },
      ) => call<ExecResult>("POST", `/v1/workspaces/${id}/exec`, req),
    },

    snapshots: {
      list: (params?: { workspace_id?: string; limit?: number; offset?: number; q?: string }) => {
        const qs0 = new URLSearchParams();
        if (params?.workspace_id)    qs0.set("workspace_id", params.workspace_id);
        if (params?.limit != null)   qs0.set("limit",        String(params.limit));
        if (params?.offset != null)  qs0.set("offset",       String(params.offset));
        if (params?.q)               qs0.set("q",            params.q);
        const qs = qs0.toString();
        return call<SnapshotList>("GET", `/v1/snapshots${qs ? `?${qs}` : ""}`);
      },
      get: (id: string) => call<SnapshotRecord>("GET", `/v1/snapshots/${id}`),
      manifest: (id: string) =>
        call<SnapshotManifest>("GET", `/v1/snapshots/${id}/manifest`),
      create: (workspaceId: string, name?: string) =>
        call<SnapshotRecord>("POST", `/v1/workspaces/${workspaceId}/snapshot`,
          name ? { name } : {}),
      delete: (id: string) => call<void>("DELETE", `/v1/snapshots/${id}`),
      restoreToNew: (
        snapshotId: string,
        parentWorkspaceId: string,
        label?: string,
      ) =>
        call<Workspace>("POST", "/v1/workspaces/from-snapshot", {
          snapshot_id: snapshotId,
          parent_workspace_id: parentWorkspaceId,
          ...(label ? { label } : {}),
        }),
    },

    settings: {
      get: () => call<SettingsSnapshot>("GET", "/v1/config"),
    },

    whoami: () => call<Whoami>("GET", "/v1/whoami"),

    flavors: {
      list: () => call<FlavorSummary[]>("GET", "/v1/flavors"),
    },
  };
}

export type Api = ReturnType<typeof createApi>;

export interface Whoami {
  tenant: string;
  role: "admin" | "operator";
}

/** A runtime flavor the server advertises for workspace creation. */
export interface FlavorSummary {
  name: string;
}
