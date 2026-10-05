import { DocPage, Section, Inline, List, Hint, Table } from "../../components/docs/DocPage";
import { Code } from "../../components/docs/Code";

export function Sdk() {
  return (
    <DocPage
      eyebrow="Build"
      title="TypeScript SDK"
      lead={
        <>
          <Inline>@agentjail/sdk</Inline> is a thin, zero-dependency HTTP client for
          the control plane. Works on Node ≥ 18 and any runtime with a global{" "}
          <Inline>fetch</Inline>. Same JSON shapes as the API — no magic.
        </>
      }
    >
      <Section id="install" title="Install">
        <Code lang="bash">npm install @agentjail/sdk</Code>
        <Code lang="ts">{`import { Agentjail } from "@agentjail/sdk";

export const aj = new Agentjail({
  baseUrl: process.env.AGENTJAIL_URL!,     // http://localhost:7070
  apiKey:  process.env.AGENTJAIL_API_KEY!, // aj_local_...
});`}</Code>
      </Section>

      <Section id="surface" title="Surface area">
        <Table
          head={["Namespace", "Methods", "What it does"]}
          rows={[
            [<Inline>credentials</Inline>, <Inline>list · put · delete</Inline>, "Manage real upstream API keys (host-only)."],
            [<Inline>sessions</Inline>,    <Inline>create · list · get · close · exec</Inline>, "Mint phantom env, optionally exec a process inside the jail."],
            [<Inline>runs</Inline>,        <Inline>create · stream · fork</Inline>, "One-shot code execution, SSE log streaming, and live-fork."],
            [<Inline>audit</Inline>,       <Inline>recent</Inline>, "Read the proxy audit log."],
          ]}
        />
      </Section>

      <Section id="credentials" title="Credentials">
        <p>
          Upload a real key. It lives on the host and is never forwarded to a sandbox.
          Re-calling <Inline>put</Inline> rotates the secret atomically.
        </p>
        <Code lang="ts">{`await aj.credentials.put({
  service: "openai",
  secret:  process.env.OPENAI_API_KEY!,
});

await aj.credentials.list();   // → [{ service: "openai", added_at: "..." }]
await aj.credentials.delete("openai");`}</Code>
        <p>Supported services today:</p>
        <List>
          <li><Inline>openai</Inline> — <Inline>https://api.openai.com</Inline></li>
          <li><Inline>anthropic</Inline> — <Inline>https://api.anthropic.com</Inline></li>
          <li><Inline>github</Inline> — <Inline>https://api.github.com</Inline></li>
          <li><Inline>stripe</Inline> — <Inline>https://api.stripe.com</Inline></li>
        </List>
      </Section>

      <Section id="sessions" title="Sessions">
        <p>
          A session bundles a set of services into a phantom env map you can hand to
          any sandbox. Phantoms expire when the session does (TTL or explicit close).
        </p>
        <Code lang="ts">{`const session = await aj.sessions.create({
  services: ["openai", "github"],
  scopes:   { github: ["/repos/my-org/*/issues*"] },  // optional path allowlist
  ttlSecs:  600,
});

// session.env →
//   OPENAI_API_KEY  = "phm_..."
//   OPENAI_BASE_URL = "http://host:8443/v1/openai/v1"
//   GITHUB_TOKEN    = "phm_..."
//   GITHUB_API_URL  = "http://host:8443/v1/github"

spawn("node", ["agent.js"], { env: { ...process.env, ...session.env } });`}</Code>
        <Hint title="Scopes are enforced at the proxy" tone="phantom">
          Requests outside the path allowlist get a 403 — the upstream is never
          contacted. Lock a phantom to one repo, one path, one glob.
        </Hint>
      </Section>

      <Section id="runs" title="Runs">
        <p>One-shot code in a fresh jail:</p>
        <Code lang="ts">{`const result = await aj.runs.create({
  code:        "console.log(1 + 1)",
  language:    "javascript",
  timeoutSecs: 5,
  memoryMb:    128,
});

console.log(result.stdout.trim());            // "2"
console.log("mem:", result.stats?.memory_peak_bytes);`}</Code>
        <p>Stream stdout/stderr line-by-line as Server-Sent Events:</p>
        <Code lang="ts">{`for await (const ev of aj.runs.stream({ code, language: "python" })) {
  switch (ev.type) {
    case "started":   console.log("pid", ev.pid);             break;
    case "stdout":    process.stdout.write(ev.line + "\\n");  break;
    case "stderr":    process.stderr.write(ev.line + "\\n");  break;
    case "completed": console.log("exit", ev.exit_code);      break;
    case "error":     console.error("fail:", ev.message);     break;
  }
}`}</Code>
        <p>
          Live-fork — spawn parent, COW-clone its output mid-run, branch evaluation
          from the same state in parallel:
        </p>
        <Code lang="ts">{`const result = await aj.runs.fork({
  language:    "javascript",
  forkAfterMs: 200,
  parentCode:  "/* expensive setup, write checkpoint to /output */",
  childCode:   "/* read /output, run alternative strategy */",
});

console.log("fork:", result.fork.clone_method, result.fork.clone_ms + "ms");`}</Code>
      </Section>

      <Section id="audit" title="Audit">
        <Code lang="ts">{`const { rows, total } = await aj.audit.recent({ limit: 50 });

for (const r of rows) {
  console.log(r.at, r.status, r.method, r.service, r.path);
}`}</Code>
      </Section>

      <Section id="errors" title="Errors">
        <p>
          All failures throw <Inline>AgentjailError</Inline> with a numeric{" "}
          <Inline>status</Inline> and the parsed JSON body when available.
        </p>
        <Code lang="ts">{`import { AgentjailError } from "@agentjail/sdk";

try {
  await aj.runs.create({ code, language: "rust" });
} catch (err) {
  if (err instanceof AgentjailError) {
    if (err.status === 400) console.error("bad request:", err.body);
    else throw err;
  }
}`}</Code>
      </Section>
    </DocPage>
  );
}
