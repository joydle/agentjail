#!/usr/bin/env -S bun run
/**
 * End-to-end smoke test for persistent workspaces + named snapshots +
 * N-way fork. Runs against a live `agentjail-server`.
 *
 * Doubles as runnable docs — every call below is a real SDK method.
 *
 *   AGENTJAIL_API_KEY=aj_local_... bun scripts/e2e-workspaces.ts
 *
 * Exits non-zero on the first assertion that fails.
 */

// Import the SDK straight from source — Bun transpiles TS on the fly so
// contributors don't need a separate `npm install` / build step to run
// the E2E. (The same code ships as the published package.)
import { Agentjail, AgentjailError } from "../packages/sdk-node/src/index.js";

const BASE_URL = process.env.CTL_URL ?? "http://localhost:7070";
const API_KEY = process.env.AGENTJAIL_API_KEY ?? undefined;

const aj = new Agentjail({ baseUrl: BASE_URL, apiKey: API_KEY });

function fail(label: string, got: unknown, want: unknown): never {
  console.error(`FAIL: ${label}: got ${JSON.stringify(got)}, want ${JSON.stringify(want)}`);
  process.exit(1);
}
function eq(label: string, got: unknown, want: unknown): void {
  if (got !== want) fail(label, got, want);
}

async function main(): Promise<void> {
  try {
    await aj.public.health();
  } catch (err) {
    if (err instanceof AgentjailError && err.status === 401) {
      console.error("! auth required — set AGENTJAIL_API_KEY");
      process.exit(2);
    }
    console.error(`! control plane not reachable at ${BASE_URL}`);
    throw err;
  }

  // 1. Create a workspace + seed a marker.
  console.log("▸ Creating workspace");
  const ws = await aj.workspaces.create({ memoryMb: 256, timeoutSecs: 30 });
  console.log(`  id: ${ws.id}`);

  const write = await aj.workspaces.exec(ws.id, {
    cmd: "/bin/sh",
    args: ["-c", "echo baseline > /workspace/marker.txt; cat /workspace/marker.txt"],
  });
  eq("exec exit_code", write.exit_code, 0);
  eq("exec stdout", write.stdout.trim(), "baseline");

  // 2. Snapshot + verify size.
  console.log("▸ Snapshot workspace");
  const snap = await aj.snapshots.create(ws.id, { name: "baseline" });
  console.log(`  id: ${snap.id} (${snap.size_bytes} bytes)`);
  if (snap.size_bytes === 0) fail("snapshot size_bytes", snap.size_bytes, "> 0");

  // 3. Mutate + verify gone.
  console.log("▸ Exec: remove file");
  const rm = await aj.workspaces.exec(ws.id, {
    cmd: "/bin/sh",
    args: ["-c", "rm /workspace/marker.txt; ls /workspace | wc -l"],
  });
  eq("rm exit_code", rm.exit_code, 0);
  eq("ls count after rm", rm.stdout.trim(), "0");

  // 4. Restore into a fresh workspace.
  console.log("▸ Restore snapshot into a new workspace");
  const restored = await aj.snapshots.createWorkspaceFrom(snap.id);
  console.log(`  new id: ${restored.id}`);
  const cat = await aj.workspaces.exec(restored.id, {
    cmd: "/bin/sh",
    args: ["-c", "cat /workspace/marker.txt"],
  });
  eq("restored stdout", cat.stdout.trim(), "baseline");

  // 5. N-way fork off the restored workspace.
  console.log("▸ Fork the restored workspace 3 ways");
  const forked = await aj.workspaces.fork(restored.id, {
    count: 3,
    label: "agents",
  });
  eq("fork count", forked.forks.length, 3);
  console.log(`  forks: ${forked.forks.map((f) => f.id).join(" ")}`);

  // 6. Each fork inherits the baseline independently.
  console.log("▸ Each fork has the baseline file");
  for (const [i, fork] of forked.forks.entries()) {
    const out = await aj.workspaces.exec(fork.id, {
      cmd: "/bin/sh",
      args: ["-c", "cat /workspace/marker.txt"],
    });
    eq(`fork ${i} stdout`, out.stdout.trim(), "baseline");
  }

  // 7. Isolation — mutating one fork doesn't affect its siblings.
  console.log("▸ Fork isolation: mutating fork-0 does not affect fork-1");
  await aj.workspaces.exec(forked.forks[0].id, {
    cmd: "/bin/sh",
    args: ["-c", "rm /workspace/marker.txt"],
  });
  const sibling = await aj.workspaces.exec(forked.forks[1].id, {
    cmd: "/bin/sh",
    args: ["-c", "cat /workspace/marker.txt"],
  });
  eq("fork-1 still has baseline", sibling.stdout.trim(), "baseline");

  // 8. Cleanup — deletes cascade via ON DELETE SET NULL; snapshots live on.
  console.log("▸ Cleanup");
  for (const fork of forked.forks) {
    await aj.workspaces.delete(fork.id);
  }
  await aj.snapshots.delete(forked.snapshot_id);
  await aj.workspaces.delete(ws.id);
  await aj.workspaces.delete(restored.id);
  await aj.snapshots.delete(snap.id);

  console.log("✓ workspaces + snapshots + fork E2E passed");
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
