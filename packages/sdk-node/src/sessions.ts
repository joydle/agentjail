import type { HttpClient } from "./http.js";
import type { ExecOptions, ExecResult, ServiceId, Session } from "./types.js";
import { encodeExecOptions } from "./exec_options.js";

/**
 * Sessions bundle a set of phantom tokens that can be handed to a sandbox.
 * Each session's tokens die when the session is closed.
 */
export class Sessions {
  constructor(private readonly http: HttpClient) {}

  /** Create a new session. Returns the full record including the env map. */
  async create(params: {
    services: ServiceId[];
    ttlSecs?: number;
    /** Per-service allow-list of path globs (`*` only supported as trailing wildcard). */
    scopes?: Partial<Record<ServiceId, string[]>>;
  }): Promise<Session> {
    const body: {
      services: ServiceId[];
      ttl_secs?: number;
      scopes?: Partial<Record<ServiceId, string[]>>;
    } = { services: params.services };
    if (params.ttlSecs !== undefined) body.ttl_secs = params.ttlSecs;
    if (params.scopes !== undefined) body.scopes = params.scopes;
    return this.http.request<Session>({
      method: "POST",
      path: "/v1/sessions",
      body,
    });
  }

  /** List every session, newest first. */
  async list(): Promise<Session[]> {
    return this.http.request<Session[]>({
      method: "GET",
      path: "/v1/sessions",
    });
  }

  /** Fetch a session by id. */
  async get(id: string): Promise<Session> {
    return this.http.request<Session>({
      method: "GET",
      path: `/v1/sessions/${encodeURIComponent(id)}`,
    });
  }

  /** Execute a command inside this session's jail. */
  async exec(
    id: string,
    params: {
      cmd: string;
      args?: string[];
      timeoutSecs?: number;
      memoryMb?: number;
    } & ExecOptions,
  ): Promise<ExecResult> {
    const body: Record<string, unknown> = { cmd: params.cmd };
    if (params.args) body.args = params.args;
    if (params.timeoutSecs !== undefined) body.timeout_secs = params.timeoutSecs;
    if (params.memoryMb !== undefined)    body.memory_mb    = params.memoryMb;
    encodeExecOptions(body, params);
    return this.http.request<ExecResult>({
      method: "POST",
      path: `/v1/sessions/${encodeURIComponent(id)}/exec`,
      body,
    });
  }

  /** Close a session. Revokes every phantom token it issued. */
  async close(id: string): Promise<void> {
    await this.http.request<void>({
      method: "DELETE",
      path: `/v1/sessions/${encodeURIComponent(id)}`,
    });
  }
}
