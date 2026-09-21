import { DocPage, Section, Inline, Hint, Table } from "../../components/docs/DocPage";
import { Code } from "../../components/docs/Code";

export function Network() {
  return (
    <DocPage
      eyebrow="Concepts"
      title="Network modes"
      lead={
        <>
          Every jail picks one of three policies. Default is{" "}
          <Inline>None</Inline> — no DNS, no sockets, no localhost. Open holes
          deliberately, never by accident.
        </>
      }
    >
      <Section id="modes" title="The three modes">
        <Table
          head={["Mode", "Reachable from inside", "Use case"]}
          rows={[
            [<Inline>None</Inline>,             "nothing",                "Builds with vendored deps, untrusted snippets, eval"],
            [<Inline>Loopback</Inline>,         "127.0.0.1 only",         "Dev servers (HMR), in-jail localhost services"],
            [<Inline>Allowlist([host…])</Inline>, "domains in the list",  "AI agents, npm install, GitHub API calls"],
          ]}
        />
      </Section>

      <Section id="setting" title="Selecting a mode">
        <p>From the SDK:</p>
        <Code lang="ts">{`await aj.runs.create({
  code,
  language: "javascript",
  network: { mode: "allowlist", domains: ["api.openai.com", "*.mcp.company.com"] },
});`}</Code>
        <p>From the Rust library:</p>
        <Code lang="rust">{`use agentjail::{Jail, JailConfig, Network};

let config = JailConfig {
    network: Network::Allowlist(vec!["api.openai.com".into()]),
    ..Default::default()
};
let jail = Jail::new(config)?;`}</Code>
      </Section>

      <Section id="allowlist" title="How allowlist mode works">
        <p>
          When a jail uses <Inline>Network::Allowlist</Inline>, the parent process
          spins up a tiny CONNECT proxy with real network access. Inside the jail,
          a veth pair gives the process a single route — to the proxy, nothing else.
          DNS is resolved by the proxy at connection time, so stale glue records
          don't matter.
        </p>
        <Code lang="text">{`        ┌─────────────────────┐
        │  jail (netns peer)  │   only route: 10.0.0.1:8443 (the proxy)
        └─────────────────────┘
                  │
                  │ HTTP CONNECT api.openai.com:443
                  ▼
        ┌─────────────────────┐
        │ allowlist proxy     │   1) match host against globs
        │ (parent process)    │   2) dial upstream
        │                     │   3) splice both halves of TCP
        └─────────────────────┘`}</Code>
        <p>
          Because the proxy speaks <Inline>HTTP CONNECT</Inline>, every TLS-based
          protocol passes through transparently — HTTPS APIs, SSE streams,
          WebSockets (so MCP works), gRPC over HTTP/2 with TLS, etc. Glob matching
          supports wildcards: <Inline>*.mcp.company.com</Inline> will accept{" "}
          <Inline>foo.mcp.company.com</Inline> but reject{" "}
          <Inline>evil-mcp.company.com.attacker.io</Inline>.
        </p>
        <Hint title="Veth setup needs CAP_NET_ADMIN" tone="flare">
          On the host, the parent that creates the veth pair needs{" "}
          <Inline>CAP_NET_ADMIN</Inline> (or root). Inside Docker, the container
          must run privileged. Cleanup is automatic via{" "}
          <Inline>PR_SET_PDEATHSIG</Inline>; if a process crashes hard you can call{" "}
          <Inline>cleanup_stale_veths()</Inline> at startup.
        </Hint>
      </Section>

      <Section id="loopback" title="Loopback in detail">
        <p>
          <Inline>Loopback</Inline> brings up <Inline>lo</Inline> inside the jail's
          network namespace and nothing else. The jail can talk to itself —
          handy for a dev server or Vite-style HMR — but cannot reach the host's
          loopback or any external network. It's the right pick for letting an
          agent run a webserver and probe its own port without exposing your
          machine.
        </p>
      </Section>
    </DocPage>
  );
}
