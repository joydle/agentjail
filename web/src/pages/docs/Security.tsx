import { DocPage, Section, Inline, Table, Hint } from "../../components/docs/DocPage";

export function Security() {
  return (
    <DocPage
      eyebrow="Reference"
      title="Security model"
      lead={
        <>
          Six isolation layers, stacked. Defense in depth — bypassing one shouldn't
          be enough. None of this is novel; what's new is having it pre-wired and
          on by default.
        </>
      }
    >
      <Section id="layers" title="The six layers">
        <Table
          head={["#", "Layer", "What it protects against"]}
          rows={[
            ["1", "Namespaces", "Isolated mount, network, IPC, PID, user — the jail can't see or signal host processes"],
            ["2", "Chroot",     "Minimal filesystem; no host /etc, /home, /root visible inside"],
            ["3", "Seccomp",    "Comprehensive syscall blocklist (io_uring, bpf, unshare, clone3, etc.)"],
            ["4", "Cgroups v2", "Memory, CPU, PIDs, I/O bandwidth — assigned before the child execs"],
            ["5", "Landlock",   "Filesystem access control (Linux 5.13+) — even reads are scoped"],
            ["6", "Hardening",  <><Inline>PR_SET_NO_NEW_PRIVS</Inline>, <Inline>RLIMIT_NOFILE</Inline>, <Inline>RLIMIT_CORE=0</Inline></>],
          ]}
        />
      </Section>

      <Section id="blocked" title="Specific attacks closed">
        <Table
          head={["Attack", "Protection"]}
          rows={[
            ["Read ~/.ssh, ~/.aws", "Not mounted"],
            ["Read /etc/shadow, ssh keys", "Minimal /etc (only ld.so, resolv.conf, ssl)"],
            ["Network exfiltration", "Network namespace + allowlist proxy"],
            ["Fork bombs", "PID limit via cgroup"],
            ["Memory exhaustion", "Memory limit + OOM detection"],
            ["Disk thrashing", "I/O bandwidth limits"],
            ["Mount manipulation", <><Inline>mount</Inline>, <Inline>mount_setattr</Inline>, new mount API blocked</>],
            ["io_uring bypass", <><Inline>io_uring_setup/enter/register</Inline> blocked</>],
            ["32-bit compat escape", <><Inline>personality()</Inline> blocked</>],
            ["Namespace escape", <><Inline>clone3</Inline>, <Inline>unshare</Inline>, <Inline>setns</Inline> blocked</>],
            ["BPF / perf abuse", <><Inline>bpf</Inline>, <Inline>perf_event_open</Inline>, <Inline>userfaultfd</Inline> blocked</>],
            ["Executable memory", <><Inline>memfd_create</Inline> blocked</>],
            ["Write+execute on /tmp", <><Inline>NOEXEC</Inline> mount flag</>],
            ["Setuid escalation", <><Inline>PR_SET_NO_NEW_PRIVS</Inline> before exec</>],
            ["Core dump leaks", <><Inline>RLIMIT_CORE=0</Inline></>],
            ["Stdout OOM of parent", "Output capped at 256 MiB per stream"],
            ["FD exhaustion", <><Inline>RLIMIT_NOFILE</Inline> capped at 4096</>],
            ["Symlink traversal", "Skipped in snapshots, forks, and cleanup"],
            ["Zombie / fd leak on crash", <><Inline>PR_SET_PDEATHSIG</Inline> + kill+reap in Drop</>],
            ["Unconstrained child", "Cgroup assigned via barrier pipe before exec"],
            ["PID reuse kill", "Reaped flag prevents killing recycled PIDs"],
          ]}
        />
      </Section>

      <Section id="phantom" title="Phantom edge — what it adds">
        <p>
          The jail makes <em>code</em> hard to escape. The phantom proxy makes{" "}
          <em>credentials</em> impossible to exfiltrate. They're complementary —
          the credential layer assumes code can be hostile and removes the prize.
        </p>
        <ul className="list-disc pl-5 marker:text-ink-600 space-y-1.5 text-[14px] text-ink-300">
          <li>Real keys never enter the jail's memory or environment</li>
          <li>Phantoms are useless off the proxy and die when the session closes</li>
          <li>Per-service path scopes restrict <em>what</em> a phantom can call, not just <em>where</em></li>
        </ul>
      </Section>

      <Section id="audit-status" title="Audit status">
        <p>
          The codebase has been through 4 rounds of security audit covering every
          source file. All critical and high severity issues have been fixed with
          regression tests. See <Inline>crates/*/tests</Inline> for specific attack
          scenarios that are verified on every build.
        </p>
        <Hint title="What this is not" tone="siren">
          agentjail is not a virtual machine. A kernel exploit could escape it,
          GPU passthrough exposes the NVIDIA driver, and trusting input to a
          privileged userspace tool always carries risk. For maximum isolation
          (multi-tenant SaaS, regulated workloads), reach for{" "}
          <a className="underline" href="https://gvisor.dev" target="_blank" rel="noreferrer">gVisor</a>{" "}
          or{" "}
          <a className="underline" href="https://firecracker-microvm.github.io" target="_blank" rel="noreferrer">Firecracker</a>{" "}
          and put agentjail inside them.
        </Hint>
      </Section>
    </DocPage>
  );
}
