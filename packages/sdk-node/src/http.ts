/**
 * Minimal HTTP client. Zero runtime deps — uses global `fetch`.
 */

/**
 * Categorical error code so callers can `switch` on the kind of failure
 * without string-matching the message. Mapped from the HTTP status by
 * {@link statusToCode}.
 */
export type AgentjailErrorCode =
  | "BAD_REQUEST"
  | "UNAUTHORIZED"
  | "FORBIDDEN"
  | "NOT_FOUND"
  | "CONFLICT"
  | "RATE_LIMITED"
  | "TIMEOUT"
  | "SERVER_ERROR"
  | "NETWORK"
  | "UNKNOWN";

function statusToCode(status: number): AgentjailErrorCode {
  if (status === 0) return "NETWORK";
  if (status === 408 || status === 504) return "TIMEOUT";
  if (status === 429) return "RATE_LIMITED";
  switch (status) {
    case 400: return "BAD_REQUEST";
    case 401: return "UNAUTHORIZED";
    case 403: return "FORBIDDEN";
    case 404: return "NOT_FOUND";
    case 409: return "CONFLICT";
  }
  if (status >= 500 && status < 600) return "SERVER_ERROR";
  if (status >= 400 && status < 500) return "BAD_REQUEST";
  return "UNKNOWN";
}

/** An error from the control plane. */
export class AgentjailError extends Error {
  public readonly status: number;
  public readonly code: AgentjailErrorCode;
  public readonly body: unknown;

  constructor(status: number, body: unknown, fallback: string) {
    const message =
      (typeof body === "object" && body !== null && "error" in body &&
        typeof (body as { error: unknown }).error === "string")
        ? (body as { error: string }).error
        : fallback;
    super(`agentjail ${status}: ${message}`);
    this.name = "AgentjailError";
    this.status = status;
    this.code = statusToCode(status);
    this.body = body;
  }
}

/** Options for an HTTP call. */
export interface RequestOptions {
  method: string;
  path: string;
  body?: unknown;
  query?: Record<string, string | number | undefined>;
  signal?: AbortSignal;
}

/** Raw fetch — used for non-JSON responses like SSE. */
export interface RawOptions {
  method: string;
  path: string;
  headers?: Record<string, string>;
  body?: string;
  signal?: AbortSignal;
}

/** Per-client HTTP config. */
export interface HttpConfig {
  /** Base URL of the control plane, no trailing slash. */
  baseUrl: string;
  /** API key sent as `Authorization: Bearer <key>`. */
  apiKey?: string;
  /** Fetch implementation. Defaults to `globalThis.fetch`. */
  fetch?: typeof fetch;
}

/** Tiny typed fetch wrapper. */
export class HttpClient {
  private readonly baseUrl: string;
  private readonly apiKey: string | undefined;
  private readonly fetchFn: typeof fetch;

  constructor(config: HttpConfig) {
    if (!config.baseUrl) {
      throw new Error("baseUrl is required");
    }
    this.baseUrl = config.baseUrl.replace(/\/+$/, "");
    this.apiKey = config.apiKey;
    this.fetchFn = config.fetch ?? globalThis.fetch;
    if (!this.fetchFn) {
      throw new Error(
        "no fetch implementation — pass one via `fetch` on Node < 18",
      );
    }
  }

  async request<T>(opts: RequestOptions): Promise<T> {
    const url = new URL(`${this.baseUrl}${opts.path}`);
    if (opts.query) {
      for (const [k, v] of Object.entries(opts.query)) {
        if (v !== undefined) url.searchParams.set(k, String(v));
      }
    }
    const headers: Record<string, string> = {
      accept: "application/json",
    };
    if (this.apiKey) {
      headers.authorization = `Bearer ${this.apiKey}`;
    }
    let bodyInit: BodyInit | undefined;
    if (opts.body !== undefined) {
      headers["content-type"] = "application/json";
      bodyInit = JSON.stringify(opts.body);
    }
    const init: RequestInit = {
      method: opts.method,
      headers,
    };
    if (bodyInit !== undefined) init.body = bodyInit;
    if (opts.signal) init.signal = opts.signal;
    const res = await this.fetchFn(url.toString(), init);
    if (res.status === 204) {
      return undefined as T;
    }
    const text = await res.text();
    // Prefer JSON, but tolerate plain-text responses (e.g. `/healthz`
    // returns the string `ok`). Falling back to the raw text keeps the
    // SDK useful for endpoints that intentionally aren't JSON without
    // forcing callers to reach for `rawFetch`.
    let parsed: unknown = null;
    if (text.length > 0) {
      try {
        parsed = JSON.parse(text);
      } catch {
        parsed = text;
      }
    }
    if (!res.ok) {
      throw new AgentjailError(res.status, parsed, res.statusText);
    }
    return parsed as T;
  }

  /**
   * Raw fetch. Returns the `Response` directly so callers can stream the
   * body (e.g. SSE). Adds `Authorization` automatically.
   */
  async rawFetch(opts: RawOptions): Promise<Response> {
    const headers: Record<string, string> = { ...(opts.headers ?? {}) };
    if (this.apiKey) headers.authorization = `Bearer ${this.apiKey}`;
    const init: RequestInit = { method: opts.method, headers };
    if (opts.body !== undefined) init.body = opts.body;
    if (opts.signal) init.signal = opts.signal;
    return this.fetchFn(`${this.baseUrl}${opts.path}`, init);
  }
}
