import type { HttpClient } from "./http.js";
import type { CredentialRecord, ServiceId } from "./types.js";

/**
 * Credential API: attach / rotate / revoke real upstream keys held by the
 * control plane. These keys never enter a sandbox; they're only used by the
 * phantom proxy to re-inject auth headers.
 */
export class Credentials {
  constructor(private readonly http: HttpClient) {}

  /** List all configured credentials (metadata only — no secrets). */
  async list(): Promise<CredentialRecord[]> {
    return this.http.request<CredentialRecord[]>({
      method: "GET",
      path: "/v1/credentials",
    });
  }

  /** Attach a new credential or rotate an existing one. */
  async put(params: {
    service: ServiceId;
    secret: string;
  }): Promise<CredentialRecord> {
    return this.http.request<CredentialRecord>({
      method: "POST",
      path: "/v1/credentials",
      body: params,
    });
  }

  /** Remove a credential. Instantly invalidates associated phantom tokens. */
  async delete(service: ServiceId): Promise<void> {
    await this.http.request<void>({
      method: "DELETE",
      path: `/v1/credentials/${encodeURIComponent(service)}`,
    });
  }
}
