import { DocPage, Section, Inline, Hint } from "../../components/docs/DocPage";
import { Code } from "../../components/docs/Code";

export function Workspaces() {
  return (
    <DocPage
      eyebrow="Concepts"
      title="Persistent workspaces"
      lead={
        <>
          A <Inline>workspace</Inline> is a long-lived mount tree that survives across
          HTTP requests. Every <Inline>POST /v1/workspaces/:id/exec</Inline>
          spawns a fresh jail against the same <Inline>source</Inline> and{" "}
          <Inline>output</Inline> directories, so filesystem mutations persist
          between commands — the &ldquo;persistent VM&rdquo; pattern expressed
          as a control-plane primitive.
        </>
      }
    >
      <Section id="why" title="Why workspaces?">
        <p>
          The one-shot <Inline>/v1/runs</Inline> endpoint is great for isolated
          scripts. Real agent workloads rarely fit in one shot — you{" "}
          <Inline>git clone</Inline>, then <Inline>bun install</Inline>, then{" "}
          <Inline>bun run lint</Inline>, then <Inline>bun test</Inline>, and
          you want the <Inline>node_modules</Inline> directory to still be there
          between calls.
        </p>
        <p>
          A workspace gives every exec the same <Inline>/workspace</Inline>{" "}
          mount. The control plane tracks it, persists its jail config, and
          publishes the cgroup path so a concurrent snapshot can freeze the
          running process for a consistent capture.
        </p>
      </Section>

      <Section id="create" title="Create">
        <p>
          Pass an optional git repo to seed the source tree + the usual
          jail knobs:
        </p>
        <Code lang="ts">{`const ws = await aj.workspaces.create({
  git:   { repo: "https://github.com/my-org/app", ref: "main" },
  label: "review-bot",
  memoryMb: 1024,
  network: { mode: "allowlist", domains: ["registry.npmjs.org"] },
});

console.log(ws.id);          // wrk_<24hex>
console.log(ws.output_dir);  // /var/lib/agentjail/workspaces/wrk_abc/output`}</Code>
        <p>
          The <Inline>source</Inline> mount holds your cloned repo (or stays
          empty). The <Inline>output</Inline> mount is where commands write —
          it&rsquo;s the subject of every snapshot.
        </p>
      </Section>

      <Section id="exec" title="Run commands">
        <p>
          Exec against a workspace id; per-call overrides fall back to the
          workspace&rsquo;s persisted defaults. Concurrent execs on the same
          workspace return <Inline>409 Conflict</Inline> — the per-workspace
          mutex keeps the filesystem coherent.
        </p>
        <Code lang="ts">{`await aj.workspaces.exec(ws.id, { cmd: "bun", args: ["install"] });
await aj.workspaces.exec(ws.id, { cmd: "bun", args: ["run", "lint"] });

const test = await aj.workspaces.exec(ws.id, {
  cmd: "bun",
  args: ["test"],
  timeoutSecs: 120,
});
console.log(test.exit_code, test.duration_ms);`}</Code>
        <Hint title="Same command twice is fast" tone="phantom">
          The second <Inline>bun install</Inline> sees the populated{" "}
          <Inline>node_modules</Inline> and exits in milliseconds. The first
          one pays the full download cost once.
        </Hint>
      </Section>

      <Section id="idle" title="Idle auto-pause">
        <p>
          Set <Inline>idleTimeoutSecs</Inline> on creation to let the server
          pause this workspace automatically after a period of inactivity.
          The reaper captures an auto-snapshot, wipes <Inline>output_dir</Inline>,
          and stores the snapshot id on the workspace. The very next{" "}
          <Inline>exec</Inline> restores in-place before running — callers
          never have to think about it.
        </p>
        <Code lang="ts">{`const ws = await aj.workspaces.create({
  idleTimeoutSecs: 60,   // pause after 60s idle
  memoryMb: 1024,
});

// …no activity for 60s…
// Background reaper snapshots the output dir + wipes it.

// Later:
await aj.workspaces.exec(ws.id, { cmd: "bun", args: ["test"] });
// \u2192 auto-resume path rehydrates before running.`}</Code>
        <Hint title="Disk reclamation" tone="phantom">
          Pausing empties the workspace&rsquo;s output dir but keeps the
          snapshot. Combine with{" "}
          <Inline>AGENTJAIL_SNAPSHOT_POOL_DIR</Inline> to get content-addressed
          dedupe across snapshots + near-free hardlink restores.
        </Hint>
      </Section>

      <Section id="list" title="List + search">
        <p>
          <Inline>GET /v1/workspaces</Inline> is paginated + searchable server-
          side. Pass <Inline>q</Inline> to match on <Inline>id</Inline>,{" "}
          <Inline>label</Inline>, or <Inline>git_repo</Inline>{" "}
          (case-insensitive substring). <Inline>total</Inline> reflects the
          filtered count.
        </p>
        <Code lang="ts">{`const { rows, total } = await aj.workspaces.list({
  q: "review-bot",
  limit: 50,
});`}</Code>
      </Section>

      <Section id="cleanup" title="Delete">
        <p>
          <Inline>DELETE /v1/workspaces/:id</Inline> soft-deletes the row and
          removes the on-disk dirs. Snapshots taken from this workspace keep
          their content — only the foreign key goes NULL.
        </p>
        <Code lang="ts">{`await aj.workspaces.delete(ws.id);`}</Code>
      </Section>
    </DocPage>
  );
}
