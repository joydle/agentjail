/**
 * Marketing showcase snippets. These are stage scripts — they demonstrate
 * *our* primitives (phantom sessions, live_fork, SSE stream, network
 * allowlist, bound jails) against real agent scenarios.
 *
 * Keep each snippet under ~20 lines. Land the point, don't tutor.
 */
export interface Showcase {
  id: string;
  label: string;
  tag: string;      // short descriptor shown under the tab
  language: "typescript";
  code: string;
}

export const SHOWCASES: Showcase[] = [
  {
    id: "builder",
    label: "App builder",
    tag: "npm install · bun run build · in a jail",
    language: "typescript",
    code: `// Build an app from scratch — inside a sealed jail with a
// narrow network hole to the npm registry only.
import { Agentjail } from "@agentjail/sdk";
const aj = new Agentjail({ baseUrl: URL, apiKey: KEY });

const result = await aj.runs.create({
  language: "bash",
  memoryMb: 1024,
  timeoutSecs: 120,
  network:    { mode: "allowlist", domains: ["registry.npmjs.org"] },
  code: \`
    set -e
    npm install --omit=dev --silent react react-dom
    echo "installed: $(ls node_modules | wc -l) packages"
  \`,
});
console.log(result.stdout.trim()); // "installed: 86 packages"
`,
  },
  {
    id: "branching",
    label: "Branching agent",
    tag: "live_fork + parallel evaluation",
    language: "typescript",
    code: `// Expensive checkpoint? Fork the running jail and evaluate
// multiple strategies in parallel. COW clone = milliseconds.
const { parent, child, fork } = await aj.runs.fork({
  language: "javascript",
  forkAfterMs: 200,
  parentCode: \`
    const fs = require("fs");
    fs.writeFileSync("/output/ckpt.json", JSON.stringify({loss: 0.42}));
    await new Promise(r => setTimeout(r, 2000));
  \`,
  childCode: \`
    const { loss } = JSON.parse(
      require("fs").readFileSync("/output/ckpt.json", "utf8")
    );
    console.log("branch A:", loss, "→", (loss * 0.85).toFixed(3));
  \`,
});

console.log(fork.method, fork.clone_ms + "ms,", fork.files_cow, "files");
`,
  },
  {
    id: "reviewer",
    label: "CI reviewer",
    tag: "test · lint · AI review · phantom OpenAI",
    language: "typescript",
    code: `// Run the whole CI pipeline in a jail. The phantom token means
// the model call never sees your real OPENAI_API_KEY.
const session = await aj.sessions.create({
  services: ["openai"], ttlSecs: 300,
});

const r = await aj.sessions.exec(session.id, {
  cmd: "bash",
  args: ["-lc", \`
    set -e
    npm ci --silent
    npm test --silent
    node ./scripts/ai-review.js < diff.patch
  \`],
  memoryMb:    2048,
  network:     { mode: "allowlist",
                 domains: ["registry.npmjs.org", "api.openai.com"] },
  seccomp:     "strict",
  timeoutSecs: 300,
});

console.log(r.exit_code, r.stats?.memory_peak_bytes);
`,
  },
  {
    id: "assistant",
    label: "Persistent agent",
    tag: "SSE stream · live stdout/stderr",
    language: "typescript",
    code: `// Stream stdout/stderr frame-by-frame into your UI as the agent
// thinks. Zero buffering, zero polling — real SSE.
for await (const ev of aj.runs.stream({
  language: "javascript",
  memoryMb: 256,
  code: \`
    (async () => {
      for (let i = 1; i <= 5; i++) {
        console.log("step", i, "of 5");
        await new Promise(r => setTimeout(r, 200));
      }
    })();
  \`,
})) {
  if (ev.type === "stdout")    render(ev.line);
  if (ev.type === "completed") done(ev.exit_code, ev.memory_peak_bytes);
}
`,
  },
];
