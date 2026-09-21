import type { HttpClient } from "./http.js";
import { AgentjailError } from "./http.js";
import type {
  ExecResult,
  ForkRequest,
  ForkResult,
  RunRequest,
  StreamEvent,
} from "./types.js";
import { encodeExecOptions } from "./exec_options.js";

/** One-shot code execution. */
export class Runs {
  constructor(private readonly http: HttpClient) {}

  /** Run code in a fresh jail and return the result. */
  async create(params: RunRequest): Promise<ExecResult> {
    const body: Record<string, unknown> = { code: params.code };
    if (params.language) body.language = params.language;
    if (params.timeoutSecs !== undefined) body.timeout_secs = params.timeoutSecs;
    if (params.memoryMb !== undefined)    body.memory_mb    = params.memoryMb;
    encodeExecOptions(body, params);
    return this.http.request<ExecResult>({
      method: "POST",
      path: "/v1/runs",
      body,
    });
  }

  /**
   * Live-fork: spawn a parent, mid-run COW-clone its output, then spawn
   * one or more children against the forked filesystem state. Pass either
   * `childCode` (single child) or `children` (up to 16). Both the legacy
   * (`child`, `fork`) and N-way (`children`, `forks`) fields are returned.
   */
  async fork(params: ForkRequest): Promise<ForkResult> {
    if (!params.childCode && !params.children) {
      throw new Error("fork: provide either childCode or children");
    }
    if (params.childCode && params.children) {
      throw new Error("fork: childCode and children are mutually exclusive");
    }
    const body: Record<string, unknown> = {
      parent_code: params.parentCode,
    };
    if (params.childCode) body.child_code = params.childCode;
    if (params.children) {
      body.children = params.children.map((c) => {
        const child: Record<string, unknown> = { code: c.code };
        if (c.memoryMb !== undefined) child.memory_mb = c.memoryMb;
        return child;
      });
    }
    if (params.language)    body.language       = params.language;
    if (params.forkAfterMs !== undefined) body.fork_after_ms = params.forkAfterMs;
    if (params.timeoutSecs  !== undefined) body.timeout_secs  = params.timeoutSecs;
    if (params.memoryMb     !== undefined) body.memory_mb     = params.memoryMb;
    encodeExecOptions(body, params);
    return this.http.request<ForkResult>({
      method: "POST",
      path: "/v1/runs/fork",
      body,
    });
  }

  /**
   * Stream stdout/stderr lines as they are produced, via Server-Sent Events.
   *
   * Usage:
   *   for await (const ev of aj.runs.stream({ code, language })) {
   *     if (ev.type === "stdout") ...
   *   }
   */
  async *stream(params: RunRequest): AsyncGenerator<StreamEvent> {
    const body: Record<string, unknown> = { code: params.code };
    if (params.language)    body.language    = params.language;
    if (params.timeoutSecs !== undefined) body.timeout_secs = params.timeoutSecs;
    if (params.memoryMb    !== undefined) body.memory_mb    = params.memoryMb;
    encodeExecOptions(body, params);

    const res = await this.http.rawFetch({
      method: "POST",
      path: "/v1/runs/stream",
      headers: { accept: "text/event-stream", "content-type": "application/json" },
      body: JSON.stringify(body),
    });

    if (!res.ok || !res.body) {
      const text = await res.text().catch(() => "");
      let parsed: unknown = null;
      try { parsed = text ? JSON.parse(text) : null; } catch { /* not JSON */ }
      throw new AgentjailError(
        res.status,
        parsed,
        text || res.statusText || "stream failed",
      );
    }

    yield* parseSSE(res.body);
  }
}

/**
 * Parse a `text/event-stream` into a stream of typed StreamEvents.
 * Tiny parser — doesn't try to handle every SSE edge case, just the shapes
 * our server actually emits (single `event:` + single `data:` per frame).
 */
async function* parseSSE(body: ReadableStream<Uint8Array>): AsyncGenerator<StreamEvent> {
  const decoder = new TextDecoder();
  const reader = body.getReader();
  let buffer = "";
  try {
    while (true) {
      const { value, done } = await reader.read();
      if (done) break;
      buffer += decoder.decode(value, { stream: true });
      // SSE frames are separated by a blank line.
      let sep: number;
      while ((sep = buffer.indexOf("\n\n")) >= 0) {
        const frame = buffer.slice(0, sep);
        buffer = buffer.slice(sep + 2);
        const parsed = parseFrame(frame);
        if (parsed) yield parsed;
      }
    }
  } finally {
    reader.releaseLock();
  }
}

function parseFrame(frame: string): StreamEvent | null {
  let type = "";
  let data = "";
  for (const raw of frame.split("\n")) {
    if (raw.startsWith(":"))             continue;     // comment / keep-alive
    else if (raw.startsWith("event:"))   type = raw.slice(6).trim();
    else if (raw.startsWith("data:"))    data += raw.slice(5).trimStart();
  }
  if (!type) return null;

  switch (type) {
    case "stdout":   return { type: "stdout", line: data };
    case "stderr":   return { type: "stderr", line: data };
    case "started":
    case "stats":
    case "completed":
    case "error": {
      try {
        const payload = JSON.parse(data);
        return { type, ...payload } as StreamEvent;
      } catch {
        return null;
      }
    }
    default: return null;
  }
}
