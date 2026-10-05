/**
 * Playground gallery — three families:
 *
 *   run      → POST /v1/runs (executes in a fresh jail from this browser)
 *   sdk      → @agentjail/sdk snippets (the public TypeScript client)
 *   advanced → Rust library features beyond the HTTP surface
 *              (live fork, event streaming, snapshots — library-only today)
 *
 * The SDK is the plug-and-play path; the advanced group exists for users who
 * want to reach directly into the Rust engine and compose primitives the
 * HTTP API doesn't expose yet.
 */

export type RecipeKind = "run" | "sdk" | "advanced";
export type RunLanguage = "javascript" | "python" | "bash";

export interface Recipe {
  id: string;
  kind: RecipeKind;
  group: string;
  title: string;
  description: string;
  code: string;
  language?: RunLanguage;
  display?: string;
  memoryMb?: number;
  timeoutSecs?: number;
}

export const RECIPES: Recipe[] = [
  // ─── Execute in-jail ───────────────────────────────────────────────────
  {
    id: "run-js-hello",
    kind: "run",
    group: "run",
    title: "Hello, jail",
    description: "Minimal smoke test — prints to stdout.",
    language: "javascript",
    display: "js",
    code: `// A fresh jail per call. No network, no fs leak, seccomp on.
console.log("hello from agentjail");
console.log("now:", new Date().toISOString());
console.log("env keys:", Object.keys(process.env).length);
`,
  },
  {
    id: "run-js-fib",
    kind: "run",
    group: "run",
    title: "Fibonacci",
    description: "CPU-bound loop — watch the cpu metric climb.",
    language: "javascript",
    display: "js",
    code: `function fib(n) {
  let a = 0n, b = 1n;
  for (let i = 0; i < n; i++) [a, b] = [b, a + b];
  return a;
}
console.log("fib(200) =", fib(200).toString());
`,
  },
  {
    id: "run-js-crypto",
    kind: "run",
    group: "run",
    title: "Crypto hash",
    description: "Uses node:crypto — stdlib is available.",
    language: "javascript",
    display: "js",
    code: `const { createHash, randomBytes } = require("node:crypto");
const payload = randomBytes(4096);
console.log("sha256:", createHash("sha256").update(payload).digest("hex"));
`,
  },
  {
    id: "run-py-probe",
    kind: "run",
    group: "run",
    title: "Sandbox probe (py)",
    description: "Inspects the jailed process — confirms isolation.",
    language: "python",
    display: "py",
    code: `import os, sys, platform
print("python  :", sys.version.split()[0])
print("uid/gid :", os.getuid(), os.getgid())
print("cwd     :", os.getcwd())
print("kernel  :", platform.release())
`,
  },
  {
    id: "run-sh-probe",
    kind: "run",
    group: "run",
    title: "Sandbox probe (sh)",
    description: "Most host paths are invisible from inside.",
    language: "bash",
    display: "sh",
    code: `id 2>/dev/null || echo "(uid unknown)"
echo "-- /"
ls / 2>&1 | head -12
`,
  },
  {
    id: "run-sh-net",
    kind: "run",
    group: "run",
    title: "Network denied",
    description: "Expect failure — /v1/runs uses Network::None.",
    language: "bash",
    display: "sh",
    code: `if getent hosts example.com >/dev/null 2>&1; then
  echo "dns resolved (unexpected)"
else
  echo "dns blocked ✓"
fi
`,
  },

  // ─── SDK: @agentjail/sdk ───────────────────────────────────────────────
  {
    id: "sdk-install",
    kind: "sdk",
    group: "sdk",
    title: "Install & connect",
    description: "npm install and construct the client.",
    display: "ts",
    code: `// npm install @agentjail/sdk
import { Agentjail } from "@agentjail/sdk";

export const aj = new Agentjail({
  baseUrl: process.env.AGENTJAIL_URL!,     // http://localhost:7070
  apiKey:  process.env.AGENTJAIL_API_KEY!, // aj_local_...
});
`,
  },
  {
    id: "sdk-attach",
    kind: "sdk",
    group: "sdk",
    title: "Attach a credential",
    description: "Upload a real key — never leaves the host again.",
    display: "ts",
    code: `await aj.credentials.put({
  service: "openai",
  secret:  process.env.OPENAI_API_KEY!,
});

// Re-calling put() rotates the key — same service, new secret.
`,
  },
  {
    id: "sdk-mint",
    kind: "sdk",
    group: "sdk",
    title: "Mint a session",
    description: "Get a bundle of phantom env vars for the sandbox.",
    display: "ts",
    code: `const session = await aj.sessions.create({
  services: ["openai", "github"],
  ttlSecs:  600,
});

// session.env looks like:
//   OPENAI_API_KEY     = "phm_..."
//   OPENAI_BASE_URL    = "http://host:8443/v1/openai/v1"
//   GITHUB_TOKEN       = "phm_..."
//   GITHUB_API_URL     = "http://host:8443/v1/github"
`,
  },
  {
    id: "sdk-scoped",
    kind: "sdk",
    group: "sdk",
    title: "Scoped session",
    description: "Lock a phantom to one repo, one path, one glob.",
    display: "ts",
    code: `const session = await aj.sessions.create({
  services: ["github"],
  scopes:   { github: ["/repos/my-org/my-repo/issues*"] },
  ttlSecs:  300,
});

// The phantom is rejected for any GitHub path outside the glob —
// even if the agent tries, the proxy will refuse.
`,
  },
  {
    id: "sdk-spawn",
    kind: "sdk",
    group: "sdk",
    title: "Spawn a sandbox",
    description: "Hand the phantom env to any child process.",
    display: "ts",
    code: `import { spawn } from "node:child_process";

const session = await aj.sessions.create({
  services: ["openai"],
  ttlSecs:  120,
});

const child = spawn("node", ["agent.js"], {
  env:   { ...process.env, ...session.env },
  stdio: "inherit",
});

child.on("exit", async (code) => {
  await aj.sessions.close(session.id);  // revoke phantom
  console.log("agent exit", code);
});
`,
  },
  {
    id: "sdk-call-upstream",
    kind: "sdk",
    group: "sdk",
    title: "Call OpenAI through phantom",
    description: "Inside the jail. Real sk-... is nowhere near.",
    display: "ts",
    code: `// This runs inside the sandbox, using the env injected by sessions.create().
const res = await fetch(\`\${process.env.OPENAI_BASE_URL}/chat/completions\`, {
  method: "POST",
  headers: {
    authorization: \`Bearer \${process.env.OPENAI_API_KEY}\`, // phm_...
    "content-type": "application/json",
  },
  body: JSON.stringify({
    model: "gpt-4o-mini",
    messages: [{ role: "user", content: "Hi." }],
  }),
});

console.log(res.status, await res.json());
`,
  },
  {
    id: "sdk-exec",
    kind: "sdk",
    group: "sdk",
    title: "Exec in a session",
    description: "Run a one-off command with the session's env baked in.",
    display: "ts",
    code: `const session = await aj.sessions.create({
  services: ["openai"], ttlSecs: 60,
});

const result = await aj.sessions.exec(session.id, {
  cmd:  "node",
  args: ["-e", "console.log('ok', Boolean(process.env.OPENAI_API_KEY))"],
  timeoutSecs: 10,
  memoryMb:    256,
});

console.log(result.exit_code, result.stdout);
`,
  },
  {
    id: "sdk-run",
    kind: "sdk",
    group: "sdk",
    title: "One-shot /v1/runs",
    description: "No session, no secrets — just run this snippet.",
    display: "ts",
    code: `const result = await aj.runs.create({
  code:        'console.log(1 + 1)',
  language:    "javascript",
  timeoutSecs: 5,
  memoryMb:    128,
});

console.log(result.stdout.trim());   // "2"
console.log("used", result.stats?.memory_peak_bytes, "bytes");
`,
  },
  {
    id: "sdk-audit",
    kind: "sdk",
    group: "sdk",
    title: "Tail the audit log",
    description: "Every phantom-proxy request is recorded here.",
    display: "ts",
    code: `const { rows, total } = await aj.audit.recent({ limit: 50 });

for (const r of rows) {
  console.log(r.at, r.status, r.method, r.service, r.path);
}
console.log("total recorded:", total);
`,
  },
  {
    id: "sdk-allowlist",
    kind: "sdk",
    group: "sdk",
    title: "Network allowlist",
    description: "Open a narrow hole in the jail — only these hosts.",
    display: "ts",
    code: `// Default is no network. Pass a NetworkSpec to open an allowlist.
const result = await aj.runs.create({
  code: \`
    const r = await fetch("https://api.openai.com/v1/models");
    console.log("status", r.status);
  \`,
  language: "javascript",
  memoryMb: 256,
  network: {
    mode: "allowlist",
    domains: ["api.openai.com", "*.mcp.company.com"],
  },
});

// Any other host is dropped at the veth before it reaches the host DNS.
`,
  },
  {
    id: "sdk-strict",
    kind: "sdk",
    group: "sdk",
    title: "Strict seccomp + tight limits",
    description: "Crank isolation: strict syscalls, 1 core, 32 PIDs.",
    display: "ts",
    code: `const result = await aj.runs.create({
  code:       'console.log("locked down")',
  language:   "javascript",
  memoryMb:   128,
  timeoutSecs: 5,
  seccomp:    "strict",   // drops io_uring, bpf, unshare, clone3, ...
  cpuPercent: 100,        // one core
  maxPids:    32,         // forbid fork-bombs
});
console.log(result.stdout.trim());
`,
  },
  {
    id: "sdk-fork",
    kind: "sdk",
    group: "sdk",
    title: "Live fork — branching evaluation",
    description: "Real scenario: checkpoint, fork, compare two strategies.",
    display: "ts",
    code: `/*
 * Real-world use: an agent runs an expensive training step that writes a
 * checkpoint to /output. Before moving on we live_fork the jail, then
 * replay the checkpoint against two candidate strategies in parallel
 * without re-running the costly setup. Whichever wins is kept.
 */
const result = await aj.runs.fork({
  language:     "javascript",
  forkAfterMs:  200,
  memoryMb:     256,
  parentCode: \`
    const fs = require("fs");
    // Simulate an expensive setup — write a checkpoint.
    fs.writeFileSync("/output/state.json", JSON.stringify({ epoch: 7, loss: 0.42 }));
    console.log("parent: wrote checkpoint");
    // Keep running so live_fork has a live handle.
    for (let i = 0; i < 40; i++) {
      await new Promise(r => setTimeout(r, 50));
    }
  \`,
  childCode: \`
    const fs = require("fs");
    const { epoch, loss } = JSON.parse(fs.readFileSync("/output/state.json", "utf8"));
    // Explore one strategy on the COW-cloned output.
    const improved = +(loss * 0.85).toFixed(4);
    console.log(\\\`child: branch A from epoch \${epoch}, loss \${loss} → \${improved}\\\`);
  \`,
});

console.log("parent:", result.parent.stdout.trim());
console.log("child :", result.child.stdout.trim());
console.log(
  "fork  :", result.fork.clone_ms + "ms ·",
  result.fork.files_cow + " files reflinked",
);
`,
  },
  {
    id: "sdk-stream",
    kind: "sdk",
    group: "sdk",
    title: "Stream stdout as SSE",
    description: "Render live logs from the jail into your UI.",
    display: "ts",
    code: `/*
 * Real-world: an agent pipeline emits progress lines. Stream them into
 * your UI instead of buffering until exit.
 */
for await (const ev of aj.runs.stream({
  language: "javascript",
  memoryMb: 128,
  code: \`
    for (let i = 1; i <= 5; i++) {
      console.log("tick", i);
      await new Promise(r => setTimeout(r, 150));
    }
    console.error("done");
  \`,
})) {
  switch (ev.type) {
    case "started":   console.log("pid", ev.pid);           break;
    case "stdout":    console.log("[out]", ev.line);        break;
    case "stderr":    console.log("[err]", ev.line);        break;
    case "completed": console.log("exit", ev.exit_code,
                          "· mem", ev.memory_peak_bytes);   break;
    case "error":     console.error("fail:", ev.message);   break;
  }
}
`,
  },

  // ─── Advanced (Rust library) ───────────────────────────────────────────
  {
    id: "adv-live-fork",
    kind: "advanced",
    group: "advanced",
    title: "Live fork a jail",
    description: "Clone a running sandbox in milliseconds (COW).",
    display: "rust",
    code: `use agentjail::{Jail, preset_agent};

let jail = Jail::new(preset_agent("./code", "./out"))?;
let handle = jail.spawn("python", &["train.py"])?;

// Freeze for sub-ms via cgroup freezer, reflink-clone via FICLONE,
// then resume. The original never notices.
let (forked, info) = jail.live_fork(Some(&handle), "/tmp/fork-output")?;
println!("forked in {:?} ({:?})", info.clone_duration, info.clone_method);

// Run something else against the *same* in-progress filesystem state.
let result = forked.run("python", &["evaluate.py"]).await?;
`,
  },
  {
    id: "adv-events",
    kind: "advanced",
    group: "advanced",
    title: "Stream events",
    description: "Real-time stdout/stderr + lifecycle hooks.",
    display: "rust",
    code: `use agentjail::{Jail, JailEvent, preset_build};

let jail = Jail::new(preset_build("./src", "./out"))?;
let (_handle, mut rx) = jail.spawn_with_events("npm", &["run", "build"])?;

while let Some(event) = rx.recv().await {
    match event {
        JailEvent::Stdout(line)            => println!("{line}"),
        JailEvent::Stderr(line)            => eprintln!("{line}"),
        JailEvent::OomKilled               => eprintln!("OOM!"),
        JailEvent::Completed { exit_code, .. } => {
            println!("done: {exit_code}");
            break;
        }
        _ => {}
    }
}
`,
  },
  {
    id: "adv-snapshot",
    kind: "advanced",
    group: "advanced",
    title: "Snapshot & restore",
    description: "Save an output tree, restore it for faster rebuilds.",
    display: "rust",
    code: `use agentjail::Snapshot;

let snap = Snapshot::create(&output_dir, &snapshot_dir)?;
println!("snapshot {} MB", snap.size_bytes() / 1024 / 1024);

// Later, before the next build:
snap.restore()?;
`,
  },
  {
    id: "adv-gpu",
    kind: "advanced",
    group: "advanced",
    title: "GPU passthrough",
    description: "CUDA/PyTorch with per-GPU isolation (experimental).",
    display: "rust",
    code: `use agentjail::{Jail, JailConfig, GpuConfig};

let config = JailConfig {
    gpu: GpuConfig { enabled: true, devices: vec![0] }, // only GPU 0
    memory_mb: 8 * 1024,
    ..preset_gpu("./train", "./ckpt")
};

let jail = Jail::new(config)?;
jail.run("python3", &["train.py"]).await?;
`,
  },
  {
    id: "adv-custom",
    kind: "advanced",
    group: "advanced",
    title: "Custom jail config",
    description: "Handroll any combination of limits and policies.",
    display: "rust",
    code: `use agentjail::{Jail, JailConfig, Network, SeccompLevel};

let config = JailConfig {
    source:        "./code".into(),
    output:        "./out".into(),
    network:       Network::Allowlist(vec!["api.anthropic.com".into()]),
    seccomp:       SeccompLevel::Strict,
    memory_mb:     1024,
    cpu_percent:   200,       // two cores
    max_pids:      128,
    io_read_mbps:  200,
    io_write_mbps: 100,
    timeout_secs:  600,
    ..Default::default()
};
let jail = Jail::new(config)?;
`,
  },
];

export const GROUPS = [
  { id: "run",      label: "Execute",  hint: "runs in a fresh jail via /v1/runs from this browser"                                              },
  { id: "sdk",      label: "SDK",      hint: "@agentjail/sdk — HTTP client for the control plane. Plug into any agent."                         },
  { id: "advanced", label: "Advanced", hint: "Rust library — open source, link directly. Low-level primitives the HTTP API doesn't expose yet." },
] as const;
