import { DocPage, Section, Inline, Hint } from "../../components/docs/DocPage";
import { Code } from "../../components/docs/Code";

export function Gateway() {
  return (
    <DocPage
      eyebrow="Concepts"
      title="Gateway (inbound hostname routing)"
      lead={
        <>
          Attach one or more public hostnames to a workspace; the server
          runs a plain HTTP reverse proxy that forwards matching traffic
          either to a caller-supplied backend URL or, for workspaces on
          an allowlist network, directly to a port bound <em>inside</em>{" "}
          the jail. Lean on purpose — no TLS, no wildcard DNS. Pair
          with upstream TLS termination (nginx, Caddy, cloud LB) for
          the public-facing leg.
        </>
      }
    >
      <Section id="enable" title="Enable">
        <p>
          Set <Inline>AGENTJAIL_GATEWAY_ADDR</Inline> on the server, then
          declare <Inline>domains</Inline> on a workspace in one of two
          shapes:
        </p>
        <Code lang="bash">{`# agentjail-server
export AGENTJAIL_GATEWAY_ADDR=0.0.0.0:8080`}</Code>
        <Code lang="ts">{`// (a) forward to a static URL you already control
await aj.workspaces.create({
  label: "preview",
  domains: [
    { domain: "review-42.preview.local", backend_url: "http://127.0.0.1:3000" },
  ],
});

// (b) forward directly to a port bound INSIDE the jail — gateway
//     resolves to the live jail IP at request time. Requires
//     network: { mode: "allowlist", ... } so the veth pair exists.
await aj.workspaces.create({
  label: "app-builder",
  network: { mode: "allowlist", domains: ["registry.npmjs.org"] },
  domains: [
    { domain: "proj-42.local.agentjail", vm_port: 3000 },
  ],
});`}</Code>
        <Hint title="Exactly one target per entry" tone="phantom">
          Each <Inline>domains[]</Inline> entry must carry{" "}
          <Inline>backend_url</Inline> <em>or</em> <Inline>vm_port</Inline>,
          never both. The create endpoint validates this up front and
          returns <Inline>400</Inline> on violations so you don&rsquo;t
          get a silent 503 at first request time.
        </Hint>
      </Section>

      <Section id="how" title="How matching works">
        <p>
          On every incoming request, the gateway trims the <Inline>:port</Inline>
          suffix from the <Inline>Host</Inline> header and compares
          case-insensitively against each workspace&rsquo;s{" "}
          <Inline>domains[].domain</Inline>. Hop-by-hop headers are stripped;
          bodies under 10&nbsp;MB are forwarded as-is. An{" "}
          <Inline>X-Forwarded-Host</Inline> header is set to the original hostname.
        </p>
        <Code lang="text">{`client                              agentjail-server                        backend
  │                                      │                                       │
  │ GET / HTTP/1.1                       │                                       │
  │ Host: review-42.preview.local:8080   │                                       │
  ├─────────────────────────────────────▶│                                       │
  │                                      │ lookup workspace.domains              │
  │                                      │ backend_url  → forward verbatim       │
  │                                      │ vm_port      → look up live jail_ip   │
  │                                      │                forward to <ip>:<port> │
  │                                      ├─────────────────────────────────────▶│
  │                                      │◀──────────────────────────────────────│
  │◀─────────────────────────────────────│                                       │`}</Code>
        <Hint title="Unmatched hosts" tone="phantom">
          Requests whose <Inline>Host</Inline> doesn&rsquo;t match any live
          workspace domain get a plain-text <Inline>404</Inline> naming the
          host. Missing-header requests get a <Inline>400</Inline>.
        </Hint>
      </Section>

      <Section id="vm-port" title="vm_port lifecycle">
        <p>
          When a workspace exec starts with allowlist networking, the
          jail gets a fresh veth pair; the jail-side IP (a private
          10.0.0.0/8 address) is published into an in-memory registry
          for the duration of the exec. Each gateway request for a{" "}
          <Inline>vm_port</Inline> domain looks up the current IP and
          rewrites the forward to{" "}
          <Inline>{"http://<jail_ip>:<vm_port>/"}</Inline>.
        </p>
        <p>
          If no exec is in flight (dev server not started yet, or
          already exited), the gateway returns{" "}
          <Inline>503 Service Unavailable</Inline> with a message naming
          the workspace + port. Start the server backgrounded inside a
          long-running exec and subsequent requests will land.
        </p>
        <Hint title="Each exec gets a fresh IP" tone="phantom">
          The veth IP changes per exec. The gateway reads the registry
          on every request, so it always resolves to the latest live
          exec — no stale-cache risk. Across fork children, each
          workspace is independent and gets its own IP.
        </Hint>
      </Section>

      <Section id="limits" title="What&rsquo;s intentionally out of scope">
        <ul className="list-disc pl-5 marker:text-ink-600 space-y-1.5 text-[14px] text-ink-300">
          <li>TLS termination — run upstream (nginx, Caddy, cloud LB) and forward plain HTTP.</li>
          <li>Wildcard DNS — domains are exact matches; add one row per subdomain.</li>
          <li>Per-workspace rate limits / auth — add at the upstream layer.</li>
          <li>Cross-host veth routing — the gateway must run in the same netns that spawns the jails. Distributed deployments front each worker with a router that&rsquo;s workspace-aware.</li>
        </ul>
        <p>
          These are deliberate omissions to keep the gateway lean. Each one
          is a well-understood add-on for a later iteration.
        </p>
      </Section>
    </DocPage>
  );
}
