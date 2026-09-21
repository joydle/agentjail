/**
 * Vitest suite for @agentjail/sdk. Uses a stub fetch so we stay hermetic.
 */

import { describe, expect, it } from "vitest";
import { Agentjail, AgentjailError } from "../src/index.js";

function fakeFetch(
  handler: (req: { url: string; init: RequestInit }) => Response,
): typeof fetch {
  return async (input, init) => {
    const url = typeof input === "string" ? input : input.toString();
    return handler({ url, init: init ?? {} });
  };
}

function json(body: unknown, init?: ResponseInit): Response {
  return new Response(JSON.stringify(body), {
    ...init,
    headers: { "content-type": "application/json", ...(init?.headers ?? {}) },
  });
}

describe("HttpClient", () => {
  it("rejects missing baseUrl", () => {
    expect(() => new Agentjail({ baseUrl: "" })).toThrow();
  });

  it("strips trailing slashes from baseUrl", async () => {
    let seen = "";
    const aj = new Agentjail({
      baseUrl: "http://api.example///",
      fetch: fakeFetch(({ url }) => {
        seen = url;
        return json([]);
      }),
    });
    await aj.credentials.list();
    expect(seen).toBe("http://api.example/v1/credentials");
  });

  it("sends Authorization when apiKey is set", async () => {
    let seenAuth: string | null = null;
    const aj = new Agentjail({
      baseUrl: "http://api",
      apiKey: "aj_test",
      fetch: fakeFetch(({ init }) => {
        const headers = new Headers(init.headers as HeadersInit);
        seenAuth = headers.get("authorization");
        return json([]);
      }),
    });
    await aj.credentials.list();
    expect(seenAuth).toBe("Bearer aj_test");
  });

  it("omits Authorization when no apiKey", async () => {
    let seenAuth: string | null = "unset";
    const aj = new Agentjail({
      baseUrl: "http://api",
      fetch: fakeFetch(({ init }) => {
        const headers = new Headers(init.headers as HeadersInit);
        seenAuth = headers.get("authorization");
        return json([]);
      }),
    });
    await aj.credentials.list();
    expect(seenAuth).toBeNull();
  });

  it("wraps non-2xx responses in AgentjailError", async () => {
    const aj = new Agentjail({
      baseUrl: "http://api",
      apiKey: "k",
      fetch: fakeFetch(() =>
        json({ error: "bad request: bad secret" }, { status: 400 })
      ),
    });
    try {
      await aj.credentials.put({ service: "openai", secret: "" });
      throw new Error("expected to throw");
    } catch (err) {
      expect(err).toBeInstanceOf(AgentjailError);
      expect((err as AgentjailError).status).toBe(400);
      expect((err as AgentjailError).message).toContain("bad secret");
    }
  });

  it.each([
    [400, "BAD_REQUEST"],
    [401, "UNAUTHORIZED"],
    [403, "FORBIDDEN"],
    [404, "NOT_FOUND"],
    [409, "CONFLICT"],
    [429, "RATE_LIMITED"],
    [504, "TIMEOUT"],
    [500, "SERVER_ERROR"],
    [502, "SERVER_ERROR"],
  ])("maps status %i to code %s", async (status, code) => {
    const aj = new Agentjail({
      baseUrl: "http://api",
      apiKey: "k",
      fetch: fakeFetch(() => json({ error: "x" }, { status })),
    });
    try {
      await aj.credentials.put({ service: "openai", secret: "x" });
      throw new Error("expected to throw");
    } catch (err) {
      expect((err as AgentjailError).code).toBe(code);
    }
  });
});

describe("Credentials", () => {
  it("PUT /v1/credentials sends service+secret as JSON", async () => {
    let method = "";
    let bodyText = "";
    const aj = new Agentjail({
      baseUrl: "http://api",
      apiKey: "k",
      fetch: fakeFetch(({ init }) => {
        method = init.method as string;
        bodyText = init.body as string;
        return json({
          service: "openai",
          added_at: "2026-04-16T12:00:00Z",
          updated_at: "2026-04-16T12:00:00Z",
          fingerprint: "deadbeefdeadbeef",
        });
      }),
    });
    const r = await aj.credentials.put({
      service: "openai",
      secret: "sk-real",
    });
    expect(method).toBe("POST");
    expect(JSON.parse(bodyText)).toEqual({
      service: "openai",
      secret: "sk-real",
    });
    expect(r.service).toBe("openai");
  });

  it("DELETE /v1/credentials/:service returns void on 204", async () => {
    let seenUrl = "";
    const aj = new Agentjail({
      baseUrl: "http://api",
      apiKey: "k",
      fetch: fakeFetch(({ url }) => {
        seenUrl = url;
        return new Response(null, { status: 204 });
      }),
    });
    await aj.credentials.delete("openai");
    expect(seenUrl).toBe("http://api/v1/credentials/openai");
  });
});

describe("Sessions", () => {
  it("renames ttlSecs to ttl_secs in the body", async () => {
    let bodyText = "";
    const aj = new Agentjail({
      baseUrl: "http://api",
      apiKey: "k",
      fetch: fakeFetch(({ init }) => {
        bodyText = init.body as string;
        return json({
          id: "sess_abc",
          created_at: "2026-04-16T00:00:00Z",
          expires_at: "2026-04-16T00:10:00Z",
          services: ["openai"],
          env: { OPENAI_API_KEY: "phm_...", OPENAI_BASE_URL: "http://p" },
        });
      }),
    });
    const r = await aj.sessions.create({
      services: ["openai"],
      ttlSecs: 600,
    });
    const parsed = JSON.parse(bodyText);
    expect(parsed).toEqual({ services: ["openai"], ttl_secs: 600 });
    expect(r.id).toBe("sess_abc");
    expect(r.env.OPENAI_API_KEY).toBeTypeOf("string");
  });

  it("omits ttl_secs when ttlSecs is undefined", async () => {
    let bodyText = "";
    const aj = new Agentjail({
      baseUrl: "http://api",
      apiKey: "k",
      fetch: fakeFetch(({ init }) => {
        bodyText = init.body as string;
        return json({
          id: "sess_abc",
          created_at: "2026-04-16T00:00:00Z",
          expires_at: null,
          services: ["openai"],
          env: {},
        });
      }),
    });
    await aj.sessions.create({ services: ["openai"] });
    expect(Object.keys(JSON.parse(bodyText))).toEqual(["services"]);
  });

  it("passes scopes through", async () => {
    let bodyText = "";
    const aj = new Agentjail({
      baseUrl: "http://api",
      apiKey: "k",
      fetch: fakeFetch(({ init }) => {
        bodyText = init.body as string;
        return json({
          id: "sess_abc",
          created_at: "2026-04-16T00:00:00Z",
          expires_at: null,
          services: ["github"],
          env: {},
        });
      }),
    });
    await aj.sessions.create({
      services: ["github"],
      scopes: { github: ["/repos/foo/*"] },
    });
    expect(JSON.parse(bodyText)).toEqual({
      services: ["github"],
      scopes: { github: ["/repos/foo/*"] },
    });
  });

  it("close sends DELETE", async () => {
    let seenMethod = "";
    let seenUrl = "";
    const aj = new Agentjail({
      baseUrl: "http://api",
      apiKey: "k",
      fetch: fakeFetch(({ url, init }) => {
        seenMethod = init.method as string;
        seenUrl = url;
        return new Response(null, { status: 204 });
      }),
    });
    await aj.sessions.close("sess_abc");
    expect(seenMethod).toBe("DELETE");
    expect(seenUrl).toBe("http://api/v1/sessions/sess_abc");
  });

  it("exec sends POST with cmd and args", async () => {
    let seenUrl = "";
    let bodyText = "";
    const aj = new Agentjail({
      baseUrl: "http://api",
      apiKey: "k",
      fetch: fakeFetch(({ url, init }) => {
        seenUrl = url;
        bodyText = init.body as string;
        return json({
          stdout: "hi\n",
          stderr: "",
          exit_code: 0,
          duration_ms: 42,
          timed_out: false,
          oom_killed: false,
        });
      }),
    });
    const r = await aj.sessions.exec("sess_1", {
      cmd: "echo",
      args: ["hi"],
      timeoutSecs: 10,
    });
    expect(seenUrl).toBe("http://api/v1/sessions/sess_1/exec");
    expect(JSON.parse(bodyText)).toEqual({
      cmd: "echo",
      args: ["hi"],
      timeout_secs: 10,
    });
    expect(r.stdout).toBe("hi\n");
    expect(r.exit_code).toBe(0);
  });
});

describe("Runs", () => {
  it("create sends POST /v1/runs with code", async () => {
    let seenUrl = "";
    let bodyText = "";
    const aj = new Agentjail({
      baseUrl: "http://api",
      apiKey: "k",
      fetch: fakeFetch(({ url, init }) => {
        seenUrl = url;
        bodyText = init.body as string;
        return json({
          stdout: "hello\n",
          stderr: "",
          exit_code: 0,
          duration_ms: 100,
          timed_out: false,
          oom_killed: false,
        });
      }),
    });
    const r = await aj.runs.create({
      code: "console.log('hello')",
      language: "javascript",
      timeoutSecs: 30,
    });
    expect(seenUrl).toBe("http://api/v1/runs");
    expect(JSON.parse(bodyText)).toEqual({
      code: "console.log('hello')",
      language: "javascript",
      timeout_secs: 30,
    });
    expect(r.stdout).toBe("hello\n");
  });

  it("forwards ExecOptions (network allowlist, seccomp, cpu, pids) in snake_case", async () => {
    let bodyText = "";
    const aj = new Agentjail({
      baseUrl: "http://api",
      apiKey: "k",
      fetch: fakeFetch(({ init }) => {
        bodyText = init.body as string;
        return json({
          stdout: "", stderr: "", exit_code: 0,
          duration_ms: 1, timed_out: false, oom_killed: false,
        });
      }),
    });
    await aj.runs.create({
      code: "fetch('https://api.openai.com')",
      language: "javascript",
      network: { mode: "allowlist", domains: ["api.openai.com"] },
      seccomp: "strict",
      cpuPercent: 200,
      maxPids: 128,
    });
    expect(JSON.parse(bodyText)).toEqual({
      code: "fetch('https://api.openai.com')",
      language: "javascript",
      network: { mode: "allowlist", domains: ["api.openai.com"] },
      seccomp: "strict",
      cpu_percent: 200,
      max_pids: 128,
    });
  });

  it("fork sends POST /v1/runs/fork with parent_code/child_code + options", async () => {
    let seenUrl = "";
    let bodyText = "";
    const aj = new Agentjail({
      baseUrl: "http://api",
      apiKey: "k",
      fetch: fakeFetch(({ url, init }) => {
        seenUrl = url;
        bodyText = init.body as string;
        const parentExec = {
          stdout: "parent\n", stderr: "", exit_code: 0,
          duration_ms: 220, timed_out: false, oom_killed: false,
        };
        const childExec = {
          stdout: "child\n", stderr: "", exit_code: 0,
          duration_ms: 180, timed_out: false, oom_killed: false,
        };
        const forkMeta = {
          clone_ms: 3, files_cloned: 1, files_cow: 1,
          bytes_cloned: 42, method: "reflink", was_frozen: true,
        };
        return json({
          parent: parentExec,
          child:    childExec,
          children: [childExec],
          fork:     forkMeta,
          forks:    [forkMeta],
        });
      }),
    });
    const r = await aj.runs.fork({
      parentCode: 'require("fs").writeFileSync("/output/ckpt","42")',
      childCode:  'console.log(require("fs").readFileSync("/output/ckpt","utf8"))',
      language: "javascript",
      forkAfterMs: 300,
      memoryMb: 256,
      seccomp: "strict",
    });
    expect(seenUrl).toBe("http://api/v1/runs/fork");
    expect(JSON.parse(bodyText)).toEqual({
      parent_code: 'require("fs").writeFileSync("/output/ckpt","42")',
      child_code:  'console.log(require("fs").readFileSync("/output/ckpt","utf8"))',
      language: "javascript",
      fork_after_ms: 300,
      memory_mb: 256,
      seccomp: "strict",
    });
    expect(r.parent.stdout).toBe("parent\n");
    expect(r.child.stdout).toBe("child\n");
    expect(r.children?.length).toBe(1);
    expect(r.children[0]).toEqual(r.child);
    expect(r.forks?.length).toBe(1);
    expect(r.forks[0]).toEqual(r.fork);
    expect(r.fork.method).toBe("reflink");
    expect(r.fork.was_frozen).toBe(true);
  });

  it("fork accepts N-way children and serializes them as children:[]", async () => {
    let bodyText = "";
    const aj = new Agentjail({
      baseUrl: "http://api",
      apiKey: "k",
      fetch: fakeFetch(({ init }) => {
        bodyText = init.body as string;
        const exec = {
          stdout: "", stderr: "", exit_code: 0,
          duration_ms: 1, timed_out: false, oom_killed: false,
        };
        const forkMeta = {
          clone_ms: 1, files_cloned: 0, files_cow: 0,
          bytes_cloned: 0, method: "reflink", was_frozen: true,
        };
        return json({
          parent: exec,
          child: exec, children: [exec, exec, exec],
          fork: forkMeta, forks: [forkMeta, forkMeta, forkMeta],
        });
      }),
    });
    const r = await aj.runs.fork({
      parentCode: "1",
      children: [
        { code: "a" },
        { code: "b", memoryMb: 128 },
        { code: "c" },
      ],
      language: "javascript",
    });
    expect(JSON.parse(bodyText)).toEqual({
      parent_code: "1",
      children: [{ code: "a" }, { code: "b", memory_mb: 128 }, { code: "c" }],
      language: "javascript",
    });
    expect(r.children.length).toBe(3);
    expect(r.forks.length).toBe(3);
  });

  it("fork rejects when neither childCode nor children is set", async () => {
    const aj = new Agentjail({
      baseUrl: "http://api",
      apiKey: "k",
      fetch: fakeFetch(() => json({})),
    });
    await expect(
      aj.runs.fork({ parentCode: "1" } as unknown as Parameters<typeof aj.runs.fork>[0]),
    ).rejects.toThrow();
  });

  it("stream parses SSE frames into typed events", async () => {
    const sse =
      `event: started\ndata: {"pid":1234}\n\n` +
      `event: stdout\ndata: hello\n\n` +
      `event: stdout\ndata: world\n\n` +
      `event: stderr\ndata: warn\n\n` +
      `event: completed\ndata: {"exit_code":0,"duration_ms":42,"timed_out":false,"oom_killed":false,"memory_peak_bytes":1048576,"cpu_usage_usec":1000}\n\n`;

    const aj = new Agentjail({
      baseUrl: "http://api",
      apiKey: "k",
      fetch: fakeFetch(() =>
        new Response(sse, {
          status: 200,
          headers: { "content-type": "text/event-stream" },
        })
      ),
    });

    const events = [];
    for await (const ev of aj.runs.stream({ code: "console.log('hi')", language: "javascript" })) {
      events.push(ev);
    }
    expect(events).toEqual([
      { type: "started", pid: 1234 },
      { type: "stdout", line: "hello" },
      { type: "stdout", line: "world" },
      { type: "stderr", line: "warn" },
      {
        type: "completed", exit_code: 0, duration_ms: 42,
        timed_out: false, oom_killed: false,
        memory_peak_bytes: 1048576, cpu_usage_usec: 1000,
      },
    ]);
  });

  it("sessions.exec forwards ExecOptions too", async () => {
    let bodyText = "";
    const aj = new Agentjail({
      baseUrl: "http://api",
      apiKey: "k",
      fetch: fakeFetch(({ init }) => {
        bodyText = init.body as string;
        return json({
          stdout: "", stderr: "", exit_code: 0,
          duration_ms: 1, timed_out: false, oom_killed: false,
        });
      }),
    });
    await aj.sessions.exec("sess_x", {
      cmd: "node",
      args: ["-e", "1+1"],
      memoryMb: 256,
      network: { mode: "loopback" },
      seccomp: "standard",
    });
    expect(JSON.parse(bodyText)).toEqual({
      cmd: "node",
      args: ["-e", "1+1"],
      memory_mb: 256,
      network: { mode: "loopback" },
      seccomp: "standard",
    });
  });
});

describe("Workspaces", () => {
  it("create POSTs with nested git block + label + memoryMb", async () => {
    let bodyText = "";
    const aj = new Agentjail({
      baseUrl: "http://api",
      apiKey: "k",
      fetch: fakeFetch(({ init }) => {
        bodyText = init.body as string;
        return json({
          id: "wrk_abc",
          created_at: "2026-04-19T00:00:00Z",
          deleted_at: null,
          source_dir: "/state/workspaces/wrk_abc/source",
          output_dir: "/state/workspaces/wrk_abc/output",
          config: {
            memory_mb: 512, timeout_secs: 300, cpu_percent: 100,
            max_pids: 64, network_mode: "none", network_domains: [],
            seccomp: "standard", idle_timeout_secs: 0,
          },
          last_exec_at: null,
          paused_at: null,
          auto_snapshot: null,
          git_repo: "https://github.com/org/repo", git_ref: "main",
          label: "ci",
        });
      }),
    });
    const ws = await aj.workspaces.create({
      git: { repo: "https://github.com/org/repo", ref: "main" },
      label: "ci",
      memoryMb: 512,
    });
    expect(JSON.parse(bodyText)).toEqual({
      git: { repo: "https://github.com/org/repo", ref: "main" },
      label: "ci",
      memory_mb: 512,
    });
    expect(ws.id).toBe("wrk_abc");
    expect(ws.config.memory_mb).toBe(512);
  });

  it("list includes limit+offset as query params", async () => {
    let seenUrl = "";
    const aj = new Agentjail({
      baseUrl: "http://api",
      apiKey: "k",
      fetch: fakeFetch(({ url }) => {
        seenUrl = url;
        return json({ rows: [], total: 0, limit: 10, offset: 20 });
      }),
    });
    await aj.workspaces.list({ limit: 10, offset: 20 });
    expect(seenUrl).toBe("http://api/v1/workspaces?limit=10&offset=20");
  });

  it("exec POSTs cmd+args and merges env if provided", async () => {
    let seenUrl = "";
    let bodyText = "";
    const aj = new Agentjail({
      baseUrl: "http://api",
      apiKey: "k",
      fetch: fakeFetch(({ url, init }) => {
        seenUrl = url;
        bodyText = init.body as string;
        return json({
          stdout: "ok\n", stderr: "", exit_code: 0,
          duration_ms: 10, timed_out: false, oom_killed: false,
        });
      }),
    });
    const out = await aj.workspaces.exec("wrk_abc", {
      cmd: "bun",
      args: ["test"],
      memoryMb: 1024,
      env: [["DEBUG", "1"]],
    });
    expect(seenUrl).toBe("http://api/v1/workspaces/wrk_abc/exec");
    expect(JSON.parse(bodyText)).toEqual({
      cmd: "bun",
      args: ["test"],
      memory_mb: 1024,
      env: [["DEBUG", "1"]],
    });
    expect(out.exit_code).toBe(0);
  });

  it("create forwards idleTimeoutSecs as snake_case", async () => {
    let bodyText = "";
    const aj = new Agentjail({
      baseUrl: "http://api",
      apiKey: "k",
      fetch: fakeFetch(({ init }) => {
        bodyText = init.body as string;
        return json({
          id: "wrk_idle",
          created_at: "2026-04-19T00:00:00Z",
          deleted_at: null,
          source_dir: "/state/workspaces/wrk_idle/source",
          output_dir: "/state/workspaces/wrk_idle/output",
          config: {
            memory_mb: 256, timeout_secs: 60, cpu_percent: 100,
            max_pids: 64, network_mode: "none", network_domains: [],
            seccomp: "standard", idle_timeout_secs: 90,
          },
          git_repo: null, git_ref: null, label: null,
          last_exec_at: null, paused_at: null, auto_snapshot: null,
        });
      }),
    });
    const ws = await aj.workspaces.create({ idleTimeoutSecs: 90 });
    expect(JSON.parse(bodyText)).toEqual({ idle_timeout_secs: 90 });
    expect(ws.config.idle_timeout_secs).toBe(90);
  });

  it("create accepts multi-repo git seed", async () => {
    let bodyText = "";
    const aj = new Agentjail({
      baseUrl: "http://api",
      apiKey: "k",
      fetch: fakeFetch(({ init }) => {
        bodyText = init.body as string;
        return json({
          id: "wrk_multi",
          created_at: "2026-04-19T00:00:00Z",
          deleted_at: null,
          source_dir: "/s", output_dir: "/o",
          config: {
            memory_mb: 512, timeout_secs: 300, cpu_percent: 100,
            max_pids: 64, network_mode: "none", network_domains: [],
            seccomp: "standard", idle_timeout_secs: 0,
          },
          git_repo: "https://github.com/org/a", git_ref: null,
          label: null, domains: [],
          last_exec_at: null, paused_at: null, auto_snapshot: null,
        });
      }),
    });
    await aj.workspaces.create({
      git: {
        repos: [
          { repo: "https://github.com/org/a" },
          { repo: "https://github.com/org/b", ref: "main", dir: "b-main" },
        ],
      },
    });
    expect(JSON.parse(bodyText)).toEqual({
      git: {
        repos: [
          { repo: "https://github.com/org/a" },
          { repo: "https://github.com/org/b", ref: "main", dir: "b-main" },
        ],
      },
    });
  });

  it("fork POSTs count + label and returns N forks", async () => {
    let seenUrl = "";
    let bodyText = "";
    const aj = new Agentjail({
      baseUrl: "http://api",
      apiKey: "k",
      fetch: fakeFetch(({ url, init }) => {
        seenUrl = url;
        bodyText = init.body as string;
        const baseConfig = {
          memory_mb: 512, timeout_secs: 300, cpu_percent: 100,
          max_pids: 64, network_mode: "none", network_domains: [],
          seccomp: "standard", idle_timeout_secs: 0,
        };
        const mk = (id: string) => ({
          id,
          created_at: "2026-04-19T00:00:00Z",
          deleted_at: null,
          source_dir: `/s/${id}`, output_dir: `/o/${id}`,
          config: baseConfig,
          git_repo: null, git_ref: null,
          label: null, domains: [],
          last_exec_at: null, paused_at: null, auto_snapshot: null,
        });
        return json({
          parent: mk("wrk_parent"),
          forks: [mk("wrk_f0"), mk("wrk_f1"), mk("wrk_f2")],
          snapshot_id: "snap_origin",
        });
      }),
    });
    const r = await aj.workspaces.fork("wrk_parent", { count: 3, label: "agents" });
    expect(seenUrl).toBe("http://api/v1/workspaces/wrk_parent/fork");
    expect(JSON.parse(bodyText)).toEqual({ count: 3, label: "agents" });
    expect(r.forks.length).toBe(3);
    expect(r.forks.map((f) => f.id)).toEqual(["wrk_f0", "wrk_f1", "wrk_f2"]);
    expect(r.snapshot_id).toBe("snap_origin");
  });

  it("fork rejects invalid count client-side", async () => {
    const aj = new Agentjail({
      baseUrl: "http://api",
      apiKey: "k",
      fetch: fakeFetch(() => json({})),
    });
    await expect(aj.workspaces.fork("wrk_x", { count: 0 })).rejects.toThrow();
    await expect(aj.workspaces.fork("wrk_x", { count: 17 })).rejects.toThrow();
  });

  it("delete sends DELETE and returns void on 204", async () => {
    let seenMethod = "";
    const aj = new Agentjail({
      baseUrl: "http://api",
      apiKey: "k",
      fetch: fakeFetch(({ init }) => {
        seenMethod = init.method as string;
        return new Response(null, { status: 204 });
      }),
    });
    await aj.workspaces.delete("wrk_abc");
    expect(seenMethod).toBe("DELETE");
  });
});

describe("Snapshots", () => {
  function snap(overrides: Partial<Record<string, unknown>> = {}): unknown {
    return {
      id: "snap_xyz",
      workspace_id: "wrk_abc",
      name: "baseline",
      created_at: "2026-04-19T00:01:00Z",
      path: "/state/snapshots/snap_xyz",
      size_bytes: 12345,
      ...overrides,
    };
  }

  it("create POSTs /v1/workspaces/:id/snapshot with optional name", async () => {
    let seenUrl = "";
    let bodyText = "";
    const aj = new Agentjail({
      baseUrl: "http://api",
      apiKey: "k",
      fetch: fakeFetch(({ url, init }) => {
        seenUrl = url;
        bodyText = init.body as string;
        return json(snap());
      }),
    });
    const r = await aj.snapshots.create("wrk_abc", { name: "baseline" });
    expect(seenUrl).toBe("http://api/v1/workspaces/wrk_abc/snapshot");
    expect(JSON.parse(bodyText)).toEqual({ name: "baseline" });
    expect(r.id).toBe("snap_xyz");
    expect(r.size_bytes).toBe(12345);
  });

  it("list forwards workspace_id as query param", async () => {
    let seenUrl = "";
    const aj = new Agentjail({
      baseUrl: "http://api",
      apiKey: "k",
      fetch: fakeFetch(({ url }) => {
        seenUrl = url;
        return json({ rows: [], total: 0, limit: 50, offset: 0 });
      }),
    });
    await aj.snapshots.list({ workspaceId: "wrk_abc" });
    expect(seenUrl).toBe("http://api/v1/snapshots?workspace_id=wrk_abc");
  });

  it("createWorkspaceFrom POSTs /v1/workspaces/from-snapshot", async () => {
    let seenUrl = "";
    let bodyText = "";
    const aj = new Agentjail({
      baseUrl: "http://api",
      apiKey: "k",
      fetch: fakeFetch(({ url, init }) => {
        seenUrl = url;
        bodyText = init.body as string;
        return json({
          id: "wrk_new",
          created_at: "2026-04-19T00:02:00Z",
          deleted_at: null,
          source_dir: "/state/workspaces/wrk_new/source",
          output_dir: "/state/workspaces/wrk_new/output",
          config: {
            memory_mb: 512, timeout_secs: 300, cpu_percent: 100,
            max_pids: 64, network_mode: "none", network_domains: [],
            seccomp: "standard", idle_timeout_secs: 0,
          },
          last_exec_at: null,
          paused_at: null,
          auto_snapshot: null,
          git_repo: null, git_ref: null, label: "recovered",
        });
      }),
    });
    const ws = await aj.snapshots.createWorkspaceFrom("snap_xyz", {
      parentWorkspaceId: "wrk_parent",
      label: "recovered",
    });
    expect(seenUrl).toBe("http://api/v1/workspaces/from-snapshot");
    expect(JSON.parse(bodyText)).toEqual({
      snapshot_id: "snap_xyz",
      parent_workspace_id: "wrk_parent",
      label: "recovered",
    });
    expect(ws.id).toBe("wrk_new");
    expect(ws.label).toBe("recovered");
  });
});

describe("Public", () => {
  it("health returns the server's plain-text reply", async () => {
    const aj = new Agentjail({
      baseUrl: "http://api",
      fetch: fakeFetch(() =>
        new Response(JSON.stringify("ok"), {
          status: 200,
          headers: { "content-type": "application/json" },
        })
      ),
    });
    const r = await aj.public.health();
    expect(r).toBe("ok");
  });

  it("stats returns live counters", async () => {
    const aj = new Agentjail({
      baseUrl: "http://api",
      fetch: fakeFetch(() =>
        json({
          active_execs: 2,
          total_execs: 100,
          sessions: 3,
          credentials: 4,
        })
      ),
    });
    const s = await aj.public.stats();
    expect(s.active_execs).toBe(2);
    expect(s.credentials).toBe(4);
  });
});

describe("Audit", () => {
  it("includes limit as a query param when set", async () => {
    let seenUrl = "";
    const aj = new Agentjail({
      baseUrl: "http://api",
      apiKey: "k",
      fetch: fakeFetch(({ url }) => {
        seenUrl = url;
        return json({ rows: [], total: 0 });
      }),
    });
    await aj.audit.recent(50);
    expect(seenUrl).toBe("http://api/v1/audit?limit=50");
  });

  it("omits limit when not set", async () => {
    let seenUrl = "";
    const aj = new Agentjail({
      baseUrl: "http://api",
      apiKey: "k",
      fetch: fakeFetch(({ url }) => {
        seenUrl = url;
        return json({ rows: [], total: 0 });
      }),
    });
    await aj.audit.recent();
    expect(seenUrl).toBe("http://api/v1/audit");
  });
});

describe("Settings", () => {
  it("GET /v1/config returns the full snapshot", async () => {
    let seenUrl = "";
    let seenMethod = "";
    const payload = {
      proxy: {
        base_url: "http://10.0.0.1:8443",
        bind_addr: "127.0.0.1:8443",
        providers: [
          { service_id: "openai", upstream_base: "https://api.openai.com", request_prefix: "/v1/openai/" },
        ],
      },
      control_plane: { bind_addr: "127.0.0.1:7000" },
      gateway: null,
      exec: { default_memory_mb: 512, default_timeout_secs: 300, max_concurrent: 16 },
      persistence: { state_dir: "/var/lib/agentjail", snapshot_pool_dir: null, idle_check_secs: 30 },
      snapshots: { gc: null },
    };
    const aj = new Agentjail({
      baseUrl: "http://api",
      apiKey: "k",
      fetch: fakeFetch(({ url, init }) => {
        seenUrl = url; seenMethod = init.method as string;
        return json(payload);
      }),
    });
    const s = await aj.settings.get();
    expect(seenMethod).toBe("GET");
    expect(seenUrl).toBe("http://api/v1/config");
    expect(s.proxy.providers).toHaveLength(1);
    expect(s.proxy.providers[0].service_id).toBe("openai");
    expect(s.exec?.default_memory_mb).toBe(512);
    expect(s.gateway).toBeNull();
  });
});

describe("Snapshots.manifest", () => {
  it("GET /v1/snapshots/:id/manifest returns incremental entries", async () => {
    let seenUrl = "";
    const aj = new Agentjail({
      baseUrl: "http://api",
      apiKey: "k",
      fetch: fakeFetch(({ url }) => {
        seenUrl = url;
        return json({
          kind: "incremental",
          entries: [
            { path: "a.txt", mode: 0o644, hash: "aa", size: 10 },
            { path: "b/c.txt", mode: 0o755, hash: "bb", size: 20 },
          ],
        });
      }),
    });
    const m = await aj.snapshots.manifest("snap_abc");
    expect(seenUrl).toBe("http://api/v1/snapshots/snap_abc/manifest");
    expect(m.kind).toBe("incremental");
    expect(m.entries).toHaveLength(2);
    expect(m.entries[0].path).toBe("a.txt");
  });

  it("returns classic kind with empty entries for full-copy snapshots", async () => {
    const aj = new Agentjail({
      baseUrl: "http://api",
      apiKey: "k",
      fetch: fakeFetch(() => json({ kind: "classic", entries: [] })),
    });
    const m = await aj.snapshots.manifest("snap_classic");
    expect(m.kind).toBe("classic");
    expect(m.entries).toEqual([]);
  });
});

describe("Workspaces.list q param", () => {
  it("sends q in the query string", async () => {
    let seenUrl = "";
    const aj = new Agentjail({
      baseUrl: "http://api",
      apiKey: "k",
      fetch: fakeFetch(({ url }) => {
        seenUrl = url;
        return json({ rows: [], total: 0, limit: 50, offset: 0 });
      }),
    });
    await aj.workspaces.list({ limit: 50, q: "review-bot" });
    expect(seenUrl).toContain("q=review-bot");
    expect(seenUrl).toContain("limit=50");
  });

  it("omits q when empty/unset", async () => {
    let seenUrl = "";
    const aj = new Agentjail({
      baseUrl: "http://api",
      apiKey: "k",
      fetch: fakeFetch(({ url }) => {
        seenUrl = url;
        return json({ rows: [], total: 0, limit: 50, offset: 0 });
      }),
    });
    await aj.workspaces.list({ limit: 50 });
    expect(seenUrl).not.toContain("q=");
  });
});

describe("Snapshots.list q param", () => {
  it("sends q alongside workspace_id", async () => {
    let seenUrl = "";
    const aj = new Agentjail({
      baseUrl: "http://api",
      apiKey: "k",
      fetch: fakeFetch(({ url }) => {
        seenUrl = url;
        return json({ rows: [], total: 0, limit: 50, offset: 0 });
      }),
    });
    await aj.snapshots.list({ workspaceId: "wrk_a", q: "baseline" });
    expect(seenUrl).toContain("q=baseline");
    expect(seenUrl).toContain("workspace_id=wrk_a");
  });
});
