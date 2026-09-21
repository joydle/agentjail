import type { HttpClient } from "./http.js";
import type { PublicStats } from "./types.js";

/** Unauthenticated endpoints — safe to hit without an API key. */
export class Public {
  constructor(private readonly http: HttpClient) {}

  /** Liveness probe. Returns the string `"ok"` when the server is up. */
  async health(): Promise<string> {
    return this.http.request<string>({ method: "GET", path: "/healthz" });
  }

  /** Live counters (active execs, total execs, session count, credential count). */
  async stats(): Promise<PublicStats> {
    return this.http.request<PublicStats>({ method: "GET", path: "/v1/stats" });
  }
}
