import { Link } from "react-router-dom";
import { DocPage, Section, Hint, Inline, List, Card, Cols } from "../../components/docs/DocPage";
import { Code } from "../../components/docs/Code";

export function Quickstart() {
  return (
    <DocPage
      eyebrow="Start"
      title="Quickstart"
      lead={
        <>
          Five minutes from <Inline>git clone</Inline> to running your first jailed
          script with phantom credentials. Linux host or Docker Desktop, nothing
          else.
        </>
      }
    >
      <Section id="prereqs" title="Prerequisites">
        <List>
          <li>Docker Desktop (macOS / Windows) or Docker Engine (Linux)</li>
          <li>An LLM API key — at least one of OpenAI, Anthropic, GitHub PAT, or Stripe</li>
          <li>~2 GB free disk for the first build</li>
        </List>
      </Section>

      <Section id="install" title="One-command install">
        <p>
          Clone the repo, set your upstream credentials, and bring up the platform stack
          (control plane + phantom proxy + Postgres):
        </p>
        <Code lang="bash">{`git clone https://github.com/bugthesystem/agentjail.git
cd agentjail

export OPENAI_API_KEY=sk-...
export AGENTJAIL_API_KEY=aj_local_$(openssl rand -hex 16)

docker compose -f docker-compose.platform.yml up --build -d`}</Code>
        <p>That brings up:</p>
        <Cols>
          <Card title="Control plane">
            HTTP API on <Inline>:7070</Inline>. Mints sessions, stores credentials, audits requests.
          </Card>
          <Card title="Phantom proxy">
            Reverse proxy on <Inline>:8443</Inline>. Swaps <Inline>phm_…</Inline> tokens
            for real keys.
          </Card>
          <Card title="Postgres">
            State for credentials, sessions, audit, jail registry.
          </Card>
        </Cols>
      </Section>

      <Section id="verify" title="Verify it works">
        <p>Health check (no auth required):</p>
        <Code lang="bash">{`curl http://localhost:7070/healthz
# → ok`}</Code>
        <p>List your stored credentials with the API key you minted:</p>
        <Code lang="bash">{`curl -H "Authorization: Bearer $AGENTJAIL_API_KEY" \\
     http://localhost:7070/v1/credentials
# → []   (empty until you attach one)`}</Code>
      </Section>

      <Section id="run" title="Run your first jail">
        <p>
          Submit a one-shot run. Code executes inside a fresh rootless namespace with
          seccomp on, no network, and a tight memory cap:
        </p>
        <Code lang="bash">{`curl -sS -X POST http://localhost:7070/v1/runs \\
  -H "Authorization: Bearer $AGENTJAIL_API_KEY" \\
  -H "content-type: application/json" \\
  -d '{
    "language":   "javascript",
    "code":       "console.log(\\"hi from jail\\")",
    "memory_mb":  128,
    "timeout_secs": 5
  }' | jq`}</Code>
        <Hint title="Use the Playground" tone="phantom">
          The same payload, point-and-click, with live syntax highlighting and an
          editor — open <Link to="/playground" className="underline">Playground</Link>{" "}
          (sign in first) or browse the recipes inline at the{" "}
          <Link to="/docs/playground" className="underline">Playground docs</Link>.
        </Hint>
      </Section>

      <Section id="next" title="Where to go next">
        <Cols>
          <Card title="TypeScript SDK →" to="/docs/sdk">
            Wrap the API in <Inline>@agentjail/sdk</Inline> and integrate into your agent.
          </Card>
          <Card title="Phantom proxy →" to="/docs/phantom">
            How sessions mint phantom tokens and why the real key never enters the jail.
          </Card>
          <Card title="Network modes →" to="/docs/network">
            <Inline>None</Inline>, <Inline>Loopback</Inline>, or domain{" "}
            <Inline>Allowlist</Inline> — which to pick.
          </Card>
          <Card title="Security model →" to="/docs/security">
            Six isolation layers and the attack surface each one closes.
          </Card>
        </Cols>
      </Section>
    </DocPage>
  );
}
