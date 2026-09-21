import type { HttpClient } from "./http.js";
import type { AuditList } from "./types.js";

/** Read the phantom-proxy audit log. */
export class Audit {
  constructor(private readonly http: HttpClient) {}

  /** Fetch the most recent audit rows. `limit` defaults to 100, max 1000. */
  async recent(limit?: number): Promise<AuditList> {
    const query: Record<string, string | number | undefined> = {};
    if (limit !== undefined) query.limit = limit;
    return this.http.request<AuditList>({
      method: "GET",
      path: "/v1/audit",
      query,
    });
  }
}
