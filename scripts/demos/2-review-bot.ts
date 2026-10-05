#!/usr/bin/env -S bun run
/**
 * Review Bot — Code Rabbit / Greptile shape.
 *
 * `workspaces.create({ git })` clones the repo into `/workspace`;
 * subsequent `workspaces.exec` calls reuse the same filesystem so
 * `bun install` artifacts stick around across commands.
 */

import { ai, aj, cleanup, ok, step } from "./_client.js";

const REPO_URL = process.env.DEMO_REPO ?? "https://github.com/bugthesystem/agentjail";
const REF      = process.env.DEMO_REF  ?? "main";

async function main(): Promise<void> {
  step(`Cloning ${REPO_URL}@${REF} into a workspace`);
  const ws = await aj.workspaces.create({
    git:  { repo: REPO_URL, ref: REF },
    memoryMb: 512,
    label:    "review-bot",
  });
  ok(`workspace ${ws.id}`);

  try {
    // Real bot would run `bun run lint` + `bun test` here — we pick
    // something lightweight + deterministic so the demo runs without
    // needing a particular toolchain in the jail image.
    step("Running `ls /workspace` as the lint stand-in");
    const lint = await aj.workspaces.exec(ws.id, {
      cmd: "/bin/sh", args: ["-c", "ls /workspace | head -20"],
    });
    ok(`lint: ${lint.stdout.split("\n").slice(0, 3).join(", ")}…`);

    step("Running `grep -r TODO` as the test stand-in");
    const test = await aj.workspaces.exec(ws.id, {
      cmd: "/bin/sh", args: ["-c", "grep -rc TODO /workspace || true"],
    });
    const failed = test.stdout.includes("FAIL"); // won't — placeholder
    ok(`test exit=${test.exit_code}`);

    step("Asking the AI for a review");
    const review = await ai(ws.id, "Review the diff for bugs");
    ok(`review: ${review}`);

    const decision = failed ? "REQUEST_CHANGES" : "APPROVE";
    console.log(`  → would post to github.pulls.createReview: ${decision}`);
  } finally {
    await cleanup({ workspaces: [ws.id] });
  }
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
