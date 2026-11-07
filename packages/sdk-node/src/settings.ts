import type { HttpClient } from "./http.js";
import type { Flavor, SettingsSnapshot, Whoami } from "./types.js";

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

  /** `GET /v1/whoami` — the tenant and role of the calling API key. */
  async whoami(): Promise<Whoami> {
    return this.http.request<Whoami>({ method: "GET", path: "/v1/whoami" });
  }

  /** `GET /v1/flavors` — runtime flavors a workspace can select. */
  async flavors(): Promise<Flavor[]> {
    return this.http.request<Flavor[]>({ method: "GET", path: "/v1/flavors" });
  }
}
