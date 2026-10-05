import type { HttpClient } from "./http.js";
import type {
  ExecResult,
  Workspace,
  WorkspaceCreateRequest,
  WorkspaceExecRequest,
  WorkspaceForkResponse,
  WorkspaceList,
} from "./types.js";
import { encodeExecOptions } from "./exec_options.js";

/**
 * Persistent workspaces — long-lived mount trees that survive across
 * multiple exec calls. Use these when you want filesystem mutations
 * (installed deps, generated files, partial builds) to persist between
 * commands, à la a VM with a working directory.
 *
 * ```ts
 * const ws = await aj.workspaces.create({
 *   git: { repo: "https://github.com/org/repo" },
 *   label: "review-bot",
 * });
 * await aj.workspaces.exec(ws.id, { cmd: "bun", args: ["install"] });
 * const test = await aj.workspaces.exec(ws.id, {
 *   cmd: "bun",
 *   args: ["test"],
 * });
 * if (test.exit_code !== 0) {
 *   const snap = await aj.snapshots.create(ws.id, { name: "failed-run" });
 * }
 * ```
 */
export class Workspaces {
  constructor(private readonly http: HttpClient) {}

  /** Create a workspace; optionally clones a git repo into its source dir. */
  async create(params: WorkspaceCreateRequest = {}): Promise<Workspace> {
    const body: Record<string, unknown> = {};
    if (params.git) {
      // Pass the seed through verbatim — the server accepts both the
      // single-repo (`{ repo, ref }`) and multi-repo (`{ repos: [] }`)
      // shapes, so just forward whichever the caller supplied.
      body.git = params.git;
    }
    if (params.label !== undefined)           body.label             = params.label;
    if (params.memoryMb !== undefined)        body.memory_mb         = params.memoryMb;
    if (params.timeoutSecs !== undefined)     body.timeout_secs      = params.timeoutSecs;
    if (params.idleTimeoutSecs !== undefined) body.idle_timeout_secs = params.idleTimeoutSecs;
    if (params.domains !== undefined)         body.domains           = params.domains;
    if (params.flavors !== undefined)         body.flavors           = params.flavors;
    encodeExecOptions(body, params);
    return this.http.request<Workspace>({
      method: "POST",
      path: "/v1/workspaces",
      body,
    });
  }

  /**
   * Paginated list, newest first. When `q` is set, filters to rows whose
   * `id`, `label`, or `git_repo` contain the needle (case-insensitive);
   * `total` reflects the filtered count.
   */
  async list(
    params: { limit?: number; offset?: number; q?: string } = {},
  ): Promise<WorkspaceList> {
    return this.http.request<WorkspaceList>({
      method: "GET",
      path: "/v1/workspaces",
      query: {
        limit:  params.limit,
        offset: params.offset,
        q:      params.q,
      },
    });
  }

  /** Fetch a workspace by id. Deleted workspaces return 404. */
  async get(id: string): Promise<Workspace> {
    return this.http.request<Workspace>({
      method: "GET",
      path: `/v1/workspaces/${encodeURIComponent(id)}`,
    });
  }

  /** Soft-delete + on-disk cleanup. Snapshots of this workspace survive. */
  async delete(id: string): Promise<void> {
    await this.http.request<void>({
      method: "DELETE",
      path: `/v1/workspaces/${encodeURIComponent(id)}`,
    });
  }

  /**
   * Run a command against the workspace's persistent filesystem. Returns
   * 409 if another exec is in flight against the same workspace.
   */
  async exec(id: string, params: WorkspaceExecRequest): Promise<ExecResult> {
    const body: Record<string, unknown> = { cmd: params.cmd };
    if (params.args)                       body.args         = params.args;
    if (params.timeoutSecs !== undefined)  body.timeout_secs = params.timeoutSecs;
    if (params.memoryMb    !== undefined)  body.memory_mb    = params.memoryMb;
    if (params.env)                        body.env          = params.env;
    return this.http.request<ExecResult>({
      method: "POST",
      path: `/v1/workspaces/${encodeURIComponent(id)}/exec`,
      body,
    });
  }

  /**
   * Atomic N-way fork of a persistent workspace — devin/cursor-style
   * "give me N parallel agents off the same starting state". Captures
   * one snapshot of the parent, spawns `count` fresh workspaces from
   * it, returns them all. Each fork is fully independent; run execs
   * against them in parallel.
   *
   * ```ts
   * const { forks } = await aj.workspaces.fork(ws.id, { count: 3 });
   * await Promise.all(forks.map((w, i) =>
   *   aj.workspaces.exec(w.id, { cmd: "do-thing", args: [String(i)] }),
   * ));
   * ```
   */
  async fork(
    id: string,
    params: { count: number; label?: string },
  ): Promise<WorkspaceForkResponse> {
    if (params.count < 1 || params.count > 16) {
      throw new Error("workspaces.fork: count must be 1..=16");
    }
    const body: Record<string, unknown> = { count: params.count };
    if (params.label !== undefined) body.label = params.label;
    return this.http.request<WorkspaceForkResponse>({
      method: "POST",
      path: `/v1/workspaces/${encodeURIComponent(id)}/fork`,
      body,
    });
  }
}
