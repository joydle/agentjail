import { seg, type HttpClient } from "./http.js";
import type { JailKind, JailRecord, JailsList, JailStatus } from "./types.js";

/** Jail-run ledger: every exec endpoint appends a record here. */
export class Jails {
  constructor(private readonly http: HttpClient) {}

  /** List most-recent jail runs, newest first. */
  async list(params?: {
    limit?: number;
    offset?: number;
    status?: JailStatus;
    kind?: JailKind;
    q?: string;
  }): Promise<JailsList> {
    return this.http.request<JailsList>({
      method: "GET",
      path:   "/v1/jails",
      query: {
        limit:  params?.limit,
        offset: params?.offset,
        status: params?.status,
        kind:   params?.kind,
        q:      params?.q || undefined,
      },
    });
  }

  /** Fetch a single jail record (full stdout/stderr, trimmed at 16 KiB). */
  async get(id: number): Promise<JailRecord> {
    return this.http.request<JailRecord>({
      method: "GET",
      path:   `/v1/jails/${seg(id)}`,
    });
  }
}
