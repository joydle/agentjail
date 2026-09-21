import type { HttpClient } from "./http.js";
import type {
  SnapshotList,
  SnapshotManifest,
  SnapshotRecord,
  Workspace,
} from "./types.js";

/**
 * Named snapshots — capture the current state of a workspace's output
 * directory and restore it into a new workspace later. Uses the engine's
 * freeze-before-copy path when a snapshot is taken mid-exec.
 *
 * ```ts
 * const snap = await aj.snapshots.create(ws.id, { name: "baseline" });
 * // …some risky exec…
 * const restored = await aj.snapshots.createWorkspaceFrom(snap.id);
 * // `restored.output_dir` now mirrors the baseline.
 * ```
 */
export class Snapshots {
  constructor(private readonly http: HttpClient) {}

  /**
   * Capture a snapshot of `workspace_id`'s output dir. If an exec is
   * currently running against the workspace, its cgroup is frozen for the
   * duration of the copy — callers see a consistent view.
   */
  async create(
    workspaceId: string,
    params: { name?: string } = {},
  ): Promise<SnapshotRecord> {
    const body: Record<string, unknown> = {};
    if (params.name !== undefined) body.name = params.name;
    return this.http.request<SnapshotRecord>({
      method: "POST",
      path: `/v1/workspaces/${encodeURIComponent(workspaceId)}/snapshot`,
      body,
    });
  }

  /**
   * List snapshots; optionally filtered to a workspace or by a
   * substring search (`q`) matching `id` / `name` / `workspace_id`.
   */
  async list(params: {
    workspaceId?: string;
    limit?: number;
    offset?: number;
    q?: string;
  } = {}): Promise<SnapshotList> {
    return this.http.request<SnapshotList>({
      method: "GET",
      path: "/v1/snapshots",
      query: {
        workspace_id: params.workspaceId,
        limit:        params.limit,
        offset:       params.offset,
        q:            params.q,
      },
    });
  }

  /** Fetch a snapshot's metadata. */
  async get(id: string): Promise<SnapshotRecord> {
    return this.http.request<SnapshotRecord>({
      method: "GET",
      path: `/v1/snapshots/${encodeURIComponent(id)}`,
    });
  }

  /**
   * List the files inside a pool-backed (incremental) snapshot. For
   * classic full-copy snapshots the response has `kind: "classic"` and
   * an empty entries array.
   */
  async manifest(id: string): Promise<SnapshotManifest> {
    return this.http.request<SnapshotManifest>({
      method: "GET",
      path: `/v1/snapshots/${encodeURIComponent(id)}/manifest`,
    });
  }

  /** Remove a snapshot + its on-disk dir. Idempotent. */
  async delete(id: string): Promise<void> {
    await this.http.request<void>({
      method: "DELETE",
      path: `/v1/snapshots/${encodeURIComponent(id)}`,
    });
  }

  /**
   * Rehydrate a snapshot into a brand-new workspace. `parentWorkspaceId`
   * is the ownership gate — it must match the snapshot's recorded
   * parent; the server returns 404 otherwise so no hints leak about
   * snapshots that belong to other tenants. The new workspace inherits
   * its parent's jail config (memory/network/flavors/etc).
   */
  async createWorkspaceFrom(
    snapshotId: string,
    params: { parentWorkspaceId: string; label?: string },
  ): Promise<Workspace> {
    const body: Record<string, unknown> = {
      snapshot_id:         snapshotId,
      parent_workspace_id: params.parentWorkspaceId,
    };
    if (params.label !== undefined) body.label = params.label;
    return this.http.request<Workspace>({
      method: "POST",
      path: "/v1/workspaces/from-snapshot",
      body,
    });
  }
}
