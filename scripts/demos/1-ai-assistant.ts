#!/usr/bin/env -S bun run
/**
 * AI Assistant — persistent workspace + idle auto-pause.
 *
 * `idleTimeoutSecs` makes the reaper auto-snapshot + wipe `source_dir`
 * once the workspace goes idle. The next exec transparently restores
 * before running, so the loop body never notices the pause.
 */

import { ai, aj, cleanup, ok, step } from "./_client.js";

async function getNextMessage(): Promise<string | null> {
  // Real apps block on a queue/webhook. For the demo, we return a
  // short scripted transcript then stop.
  const scripted = [
    "what's the time?",
    "summarize this in 3 bullets",
    "goodbye",
  ];
  const next = scripted[demo.cursor++];
  if (!next) return null;
  await new Promise((r) => setTimeout(r, 300));
  return next;
}

const demo = { cursor: 0 };

async function main(): Promise<void> {
  step("Creating persistent workspace with 60s idle pause");
  const ws = await aj.workspaces.create({
    idleTimeoutSecs: 60,
    memoryMb: 512,
    label: "assistant",
  });
  ok(`workspace ${ws.id} (paused? ${ws.paused_at !== null})`);

  try {
    while (true) {
      const msg = await getNextMessage();
      if (msg === null) break;

      step(`user → ${msg}`);
      // If the reaper paused the workspace since the last exec, this
      // call auto-restores the source_dir from the auto-snapshot
      // before running. Zero extra code in the loop body.
      const reply = await ai(ws.id, msg);
      ok(`assistant ← ${reply}`);

      // Check whether the workspace is currently paused — useful for
      // dashboards. The handler clears `paused_at` after auto-resume.
      const refreshed = await aj.workspaces.get(ws.id);
      if (refreshed.paused_at === null && refreshed.auto_snapshot === null) {
        console.log(`  workspace ${ws.id} is active`);
      }
    }
  } finally {
    await cleanup({ workspaces: [ws.id] });
  }
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
