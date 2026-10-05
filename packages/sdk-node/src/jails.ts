import type { HttpClient } from "./http.js";
import type { JailRecord, JailsList, JailStatus } from "./types.js";

/** Jail-run ledger: every exec endpoint appends a record here. */
export class Jails {
  constructor(private readonly http: HttpClient) {}

  /** List most-recent jail runs, newest first. */
  async list(params?: { limit?: number; status?: JailStatus }): Promise<JailsList> {
    const query: Record<string, string | number> = {};
    if (params?.limit)  query.limit  = params.limit;
    if (params?.status) query.status = params.status;
    return this.http.request<JailsList>({
      method: "GET",
      path:   "/v1/jails",
      query,
    });
  }

  /** Fetch a single jail record (full stdout/stderr, trimmed at 16 KiB). */
  async get(id: number): Promise<JailRecord> {
    return this.http.request<JailRecord>({
      method: "GET",
      path:   `/v1/jails/${id}`,
    });
  }
}
