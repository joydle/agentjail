import type { HttpClient } from "./http.js";
import type { SettingsSnapshot } from "./types.js";

/**
 * Settings — read-only snapshot of the running control plane's
 * configuration. Safe-to-display fields only (bind addresses, GC
 * policy, provider metadata). Credentials never appear here.
 */
export class Settings {
  constructor(private readonly http: HttpClient) {}

  /** `GET /v1/config` — the full settings snapshot. */
  async get(): Promise<SettingsSnapshot> {
    return this.http.request<SettingsSnapshot>({
      method: "GET",
      path: "/v1/config",
    });
  }
}
