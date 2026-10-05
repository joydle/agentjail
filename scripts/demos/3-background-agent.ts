#!/usr/bin/env -S bun run
/**
 * Background Agent — Devin / Cursor Agent shape.
 *
 * `workspaces.fork({ count, label? })` captures one snapshot of the
 * parent (freezing any in-flight exec for consistency), spawns N
 * independent workspaces from it, and returns them all. Each fork has
 * its own `source_dir` and exec mutex, so `Promise.all` is safe.
 */

import { ai, aj, cleanup, ok, step } from "./_client.js";

const REPO_URL = process.env.DEMO_REPO ?? "https://github.com/bugthesystem/agentjail";

async function main(): Promise<void> {
  step(`Cloning ${REPO_URL} into the parent workspace`);
  const parent = await aj.workspaces.create({
    git: { repo: REPO_URL },
    memoryMb: 512,
    label: "bg-agent-parent",
  });
  ok(`parent ${parent.id}`);

  try {
    step("Forking 3 ways");
    const { forks, snapshot_id } = await aj.workspaces.fork(parent.id, {
      count: 3,
      label: "agents",
    });
    ok(`forks ${forks.map((f) => f.id).join(" ")}  (snapshot ${snapshot_id})`);

    step("Dispatching 3 parallel tasks");
    const prompts = [
      "Build the API endpoints",
      "Build the frontend UI",
      "Write the test suite",
    ];
    const results = await Promise.all(
      forks.map((fork, i) => ai(fork.id, prompts[i]!)),
    );
    results.forEach((r, i) => console.log(`  fork[${i}] ← ${r}`));

    await cleanup({
      workspaces: [parent.id, ...forks.map((f) => f.id)],
      snapshots:  [snapshot_id],
    });
  } catch (err) {
    await cleanup({ workspaces: [parent.id] });
    throw err;
  }
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
