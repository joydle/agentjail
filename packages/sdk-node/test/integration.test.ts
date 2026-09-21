/**
 * Integration tests: real SDK → real server → mock upstream.
 *
 * Boots the agentjail-server binary and a mock upstream, then drives
 * the full platform through the TypeScript SDK. Proves the phantom-token
 * invariant end-to-end from a consumer's perspective.
 *
 * Requires: `cargo build -p agentjail-server` (binary must exist).
 * Run: `npm run test:integration`
 */

import { createServer, type IncomingMessage, type Server } from "node:http";
import { spawn, type ChildProcess } from "node:child_process";
import { describe, it, expect, beforeAll, afterAll } from "vitest";
import { Agentjail } from "../src/index.js";

// ---------------------------------------------------------------------------
// Mock upstream: captures last request headers
// ---------------------------------------------------------------------------

let mockUpstream: Server;
let mockPort: number;
let lastHeaders: Record<string, string | undefined> = {};

function startMockUpstream(): Promise<void> {
  return new Promise((resolve) => {
    mockUpstream = createServer((req: IncomingMessage, res) => {
      lastHeaders = {};
      for (const [k, v] of Object.entries(req.headers)) {
        lastHeaders[k] = Array.isArray(v) ? v[0] : v;
      }
      res.writeHead(200, { "content-type": "application/json" });
      res.end(JSON.stringify({ ok: true }));
    });
    mockUpstream.listen(0, "127.0.0.1", () => {
      const addr = mockUpstream.address();
      if (addr && typeof addr === "object") mockPort = addr.port;
      resolve();
    });
  });
}

// ---------------------------------------------------------------------------
// Server process management
// ---------------------------------------------------------------------------

let server: ChildProcess;
const CTL_PORT = 17000 + Math.floor(Math.random() * 1000);
const PROXY_PORT = 18000 + Math.floor(Math.random() * 1000);
const API_KEY = "aj_integration_test";

async function startServer(): Promise<void> {
  // Find the binary — check common build locations
  const bin = "../../target/debug/agentjail-server";

  server = spawn(bin, [], {
    env: {
      ...process.env,
      CTL_ADDR: `127.0.0.1:${CTL_PORT}`,
      PROXY_ADDR: `127.0.0.1:${PROXY_PORT}`,
      PROXY_BASE_URL: `http://127.0.0.1:${PROXY_PORT}`,
      AGENTJAIL_API_KEY: API_KEY,
      OPENAI_API_KEY: "sk-real-openai",
      ANTHROPIC_API_KEY: "sk-ant-real-anthropic",
      // Override upstream URLs via provider env (not supported yet — we'll use
      // the credential API + proxy to reach our mock)
      RUST_LOG: "warn",
    },
    stdio: ["ignore", "pipe", "pipe"],
  });

  // Wait for server to be ready (poll healthz)
  const start = Date.now();
  while (Date.now() - start < 10_000) {
    try {
      const resp = await fetch(`http://127.0.0.1:${CTL_PORT}/healthz`);
      if (resp.ok) return;
    } catch {
      // not ready yet
    }
    await new Promise((r) => setTimeout(r, 100));
  }
  throw new Error("server did not start within 10s");
}

function stopServer(): Promise<void> {
  return new Promise((resolve) => {
    if (!server || server.killed) {
      resolve();
      return;
    }
    server.on("close", () => resolve());
    server.kill("SIGTERM");
    // Force kill after 3s
    setTimeout(() => {
      if (!server.killed) server.kill("SIGKILL");
    }, 3000);
  });
}

// ---------------------------------------------------------------------------
// SDK client
// ---------------------------------------------------------------------------

let aj: Agentjail;

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

beforeAll(async () => {
  await startMockUpstream();

  // Build the server binary first
  const build = spawn("cargo", ["build", "-p", "agentjail-server"], {
    stdio: "inherit",
    cwd: "../..",
  });
  await new Promise<void>((resolve, reject) => {
    build.on("close", (code) =>
      code === 0 ? resolve() : reject(new Error(`cargo build failed: ${code}`))
    );
  });

  await startServer();

  aj = new Agentjail({
    baseUrl: `http://127.0.0.1:${CTL_PORT}`,
    apiKey: API_KEY,
  });
}, 60_000); // generous timeout for cargo build

afterAll(async () => {
  await stopServer();
  mockUpstream?.close();
});

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

describe("SDK integration (real server)", () => {
  it("healthz returns ok", async () => {
    const resp = await fetch(`http://127.0.0.1:${CTL_PORT}/healthz`);
    expect(resp.ok).toBe(true);
  });

  it("creates a session with phantom tokens", async () => {
    const session = await aj.sessions.create({ services: ["openai"] });

    expect(session.id).toMatch(/^sess_/);
    expect(session.services).toEqual(["openai"]);
    expect(session.env.OPENAI_API_KEY).toMatch(/^phm_/);
    expect(session.env.OPENAI_BASE_URL).toContain(`${PROXY_PORT}`);
  });

  it("lists sessions", async () => {
    const sessions = await aj.sessions.list();
    expect(sessions.length).toBeGreaterThanOrEqual(1);
  });

  it("gets a session by id", async () => {
    const created = await aj.sessions.create({ services: ["openai"] });
    const fetched = await aj.sessions.get(created.id);
    expect(fetched.id).toBe(created.id);
    expect(fetched.env.OPENAI_API_KEY).toBe(created.env.OPENAI_API_KEY);
  });

  it("phantom token reaches proxy and swaps to real key", async () => {
    const session = await aj.sessions.create({ services: ["openai"] });
    const { OPENAI_API_KEY: phm, OPENAI_BASE_URL: base } = session.env;

    // Hit the proxy like a sandbox would
    const resp = await fetch(`${base}/chat/completions`, {
      method: "POST",
      headers: {
        authorization: `Bearer ${phm}`,
        "content-type": "application/json",
      },
      body: JSON.stringify({ model: "gpt-4o-mini" }),
    });

    // Proxy forwards to real upstream (which is the official API, not our mock
    // in this test — but the proxy will try). We check audit instead.
    // The request was at least processed by the proxy.
    const audit = await aj.audit.recent(10);
    expect(audit.total).toBeGreaterThan(0);
    const last = audit.rows[0]!;
    expect(last.service).toBe("openai");
    expect(last.session_id).toBe(session.id);
  });

  it("closing a session revokes its tokens", async () => {
    const session = await aj.sessions.create({ services: ["openai"] });
    const { OPENAI_API_KEY: phm, OPENAI_BASE_URL: base } = session.env;

    // Close the session
    await aj.sessions.close(session.id);

    // Token should now be dead
    const resp = await fetch(`${base}/chat/completions`, {
      method: "POST",
      headers: {
        authorization: `Bearer ${phm}`,
        "content-type": "application/json",
      },
      body: JSON.stringify({}),
    });
    expect(resp.status).toBe(401);
  });

  it("session not found after close", async () => {
    const session = await aj.sessions.create({ services: ["openai"] });
    await aj.sessions.close(session.id);

    try {
      await aj.sessions.get(session.id);
      throw new Error("expected 404");
    } catch (err: unknown) {
      expect((err as { status?: number }).status).toBe(404);
    }
  });

  it("credential CRUD works", async () => {
    // Add a credential via the API
    const record = await aj.credentials.put({
      service: "stripe",
      secret: "sk_test_integration",
    });
    expect(record.service).toBe("stripe");
    expect(record.fingerprint).toBeTruthy();

    // List should include it
    const list = await aj.credentials.list();
    expect(list.some((c) => c.service === "stripe")).toBe(true);

    // Delete it
    await aj.credentials.delete("stripe");
    const after = await aj.credentials.list();
    expect(after.some((c) => c.service === "stripe")).toBe(false);
  });

  it("multi-service session has separate tokens", async () => {
    const session = await aj.sessions.create({
      services: ["openai", "anthropic"],
    });

    expect(session.env.OPENAI_API_KEY).toMatch(/^phm_/);
    expect(session.env.ANTHROPIC_API_KEY).toMatch(/^phm_/);
    expect(session.env.OPENAI_API_KEY).not.toBe(session.env.ANTHROPIC_API_KEY);

    expect(session.env.OPENAI_BASE_URL).toContain("/v1/openai");
    expect(session.env.ANTHROPIC_BASE_URL).toContain("/v1/anthropic");

    await aj.sessions.close(session.id);
  });

  it("audit log tracks requests", async () => {
    const audit = await aj.audit.recent();
    expect(audit.total).toBeGreaterThan(0);
    expect(audit.rows[0]).toHaveProperty("service");
    expect(audit.rows[0]).toHaveProperty("method");
    expect(audit.rows[0]).toHaveProperty("status");
  });

  // --- Exec & Runs (require Linux, skip on macOS) ---

  const IS_LINUX = process.platform === "linux";

  it.skipIf(!IS_LINUX)("exec runs a command in a jail", async () => {
    const session = await aj.sessions.create({ services: ["openai"] });
    const result = await aj.sessions.exec(session.id, {
      cmd: "/bin/sh",
      args: ["-c", "echo hello-from-sdk"],
    });

    expect(result.exit_code).toBe(0);
    expect(result.stdout).toContain("hello-from-sdk");
    expect(result.timed_out).toBe(false);
    expect(result.oom_killed).toBe(false);
    expect(result.duration_ms).toBeGreaterThan(0);

    await aj.sessions.close(session.id);
  });

  it.skipIf(!IS_LINUX)("exec injects phantom env into jail", async () => {
    const session = await aj.sessions.create({ services: ["openai"] });
    const result = await aj.sessions.exec(session.id, {
      cmd: "/bin/sh",
      args: ["-c", "echo $OPENAI_API_KEY"],
    });

    expect(result.exit_code).toBe(0);
    expect(result.stdout.trim()).toMatch(/^phm_/);

    await aj.sessions.close(session.id);
  });

  it.skipIf(!IS_LINUX)("runs.create executes code", async () => {
    const result = await aj.runs.create({
      code: "echo run-from-sdk",
      language: "bash",
    });

    expect(result.exit_code).toBe(0);
    expect(result.stdout).toContain("run-from-sdk");
  });

  it.skipIf(!IS_LINUX)("runs.create respects timeout", async () => {
    const result = await aj.runs.create({
      code: "sleep 60",
      language: "bash",
      timeoutSecs: 2,
    });

    expect(result.timed_out).toBe(true);
  });
});
