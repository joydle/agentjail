import { DocPage, Section, Inline, Hint } from "../../components/docs/DocPage";
import { Code } from "../../components/docs/Code";

export function Forking() {
  return (
    <DocPage
      eyebrow="Concepts"
      title="Live forking"
      lead={
        <>
          Freeze a running jail for sub-millisecond, copy-on-write the output tree
          via <Inline>FICLONE</Inline>, and resume — the original never notices.
          Branch evaluation cheaply.
        </>
      }
    >
      <Section id="why" title="Why fork instead of re-run?">
        <p>
          The expensive part of an agent's run is usually the setup: install deps,
          download a model, train an epoch, build an index. If you want to compare
          two strategies that share that setup, re-running it twice doubles your
          wallclock cost. Forking lets you reach the interesting state{" "}
          <em>once</em> and then diverge.
        </p>
        <p>Concretely, this fits:</p>
        <ul className="list-disc pl-5 marker:text-ink-600 space-y-1.5 text-[14px] text-ink-300">
          <li>Branching evaluation — try strategy A and strategy B against the same epoch checkpoint</li>
          <li>Speculative execution — start the next step early; throw it away if it diverges</li>
          <li>Crash-mode replay — fork right before the failing call, swap inputs, re-run</li>
        </ul>
      </Section>

      <Section id="how" title="How it works">
        <p>The fork happens in three steps, total wallclock typically under 5 ms:</p>
        <Code lang="text">{`1. Cgroup freezer suspends the parent process tree   (<1ms)
2. FICLONE ioctl reflinks /output → /output-fork     (~1ms on btrfs/xfs)
3. Cgroup freezer thaws the parent                    (<1ms)
                                                      ─────
                                                      ~3 ms`}</Code>
        <p>
          On filesystems without reflink support (ext4, tmpfs), it falls back to a
          regular recursive copy — slower, but the API is identical and{" "}
          <Inline>info.clone_method</Inline> will say <Inline>"copy"</Inline> instead
          of <Inline>"reflink"</Inline>.
        </p>
        <Hint title="The parent never sees the freeze" tone="phantom">
          The freezer pauses every task in the parent's cgroup at a syscall boundary.
          From the parent's perspective, that one syscall just happened to take a
          few ms longer than usual.
        </Hint>
      </Section>

      <Section id="sdk" title="From the SDK">
        <p>
          One call hands you both the parent and child results. Use{" "}
          <Inline>forkAfterMs</Inline> to wait for the parent to write its
          checkpoint before forking:
        </p>
        <Code lang="ts">{`const result = await aj.runs.fork({
  language:    "javascript",
  forkAfterMs: 200,
  memoryMb:    256,
  parentCode: \`
    const fs = require("fs");
    fs.writeFileSync("/output/state.json", JSON.stringify({ epoch: 7, loss: 0.42 }));
    console.log("parent: wrote checkpoint");
    // keep alive long enough for the fork to land
    (async () => { for (let i = 0; i < 40; i++) await new Promise(r => setTimeout(r, 50)); })();
  \`,
  childCode: \`
    const { epoch, loss } = JSON.parse(require("fs").readFileSync("/output/state.json", "utf8"));
    const improved = +(loss * 0.85).toFixed(4);
    console.log("child: branch A from epoch " + epoch + ", loss " + loss + " → " + improved);
  \`,
});

console.log("fork:", result.fork.clone_method, result.fork.clone_ms + "ms",
            "·", result.fork.files_cow, "files reflinked");`}</Code>
      </Section>

      <Section id="library" title="From the Rust library">
        <p>
          For more control — multiple forks from the same parent, hand-rolled
          execution after the fork — drop down to the library API:
        </p>
        <Code lang="rust">{`use agentjail::{Jail, preset_agent};

let jail = Jail::new(preset_agent("./code", "./out"))?;
let handle = jail.spawn("python", &["train.py"])?;

let (forked, info) = jail.live_fork(Some(&handle), "/tmp/fork-output")?;
println!("forked in {:?} ({:?})", info.clone_duration, info.clone_method);

// run something else against the same in-progress filesystem state
let result = forked.run("python", &["evaluate.py"]).await?;`}</Code>
      </Section>

      <Section id="limits" title="Limits and caveats">
        <ul className="list-disc pl-5 marker:text-ink-600 space-y-1.5 text-[14px] text-ink-300">
          <li>The fork only clones the output tree, not memory — the child is a fresh process</li>
          <li>Open file descriptors and in-flight network connections are <em>not</em> inherited</li>
          <li>FICLONE is btrfs/xfs only (and tmpfs in recent kernels); other FS falls back to copy</li>
          <li>Multiple forks from one parent are independent; they don't see each other's writes</li>
        </ul>
      </Section>
    </DocPage>
  );
}
