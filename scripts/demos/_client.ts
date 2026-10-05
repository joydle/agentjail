/**
 * Shared SDK client + helpers for the demo scripts. Keeps each demo
 * file focused on the *pattern* rather than wiring.
 *
 * Run any demo with:
 *   AGENTJAIL_API_KEY=... bun scripts/demos/<name>.ts
 */

import { Agentjail } from "../../packages/sdk-node/src/index.js";

export const aj = new Agentjail({
  baseUrl: process.env.CTL_URL ?? "http://localhost:7070",
  apiKey: process.env.AGENTJAIL_API_KEY ?? undefined,
});

/**
 * Placeholder `ai(workspaceId, prompt)` helper. Executes a stubbed
 * command inside the workspace — real deployments would run a script
 * that calls Claude (or similar) through the phantom proxy. Shape:
 * input → exec → text output.
 *
 * The phantom proxy wires sessions to workspaces via env vars; wire
 * those in at your call site (not shown here) to swap the placeholder
 * for a real `anthropic.messages.create(...)` call.
 */
export async function ai(workspaceId: string, prompt: string): Promise<string> {
  const safe = prompt.replace(/"/g, '\\"');
  const r = await aj.workspaces.exec(workspaceId, {
    cmd: "/bin/sh",
    args: ["-c", `echo "ai: ${safe}"`],
  });
  return r.stdout.trim();
}

/** Small typed logger so every demo prints the same way. */
export function step(msg: string, extra?: unknown): void {
  const tail = extra === undefined ? "" : ` ${JSON.stringify(extra)}`;
  console.log(`\u25b8 ${msg}${tail}`);
}

export function ok(msg: string): void {
  console.log(`\u2713 ${msg}`);
}

/** Best-effort cleanup — never throws. */
export async function cleanup(ids: { workspaces?: string[]; snapshots?: string[] }): Promise<void> {
  for (const id of ids.workspaces ?? []) {
    try {
      await aj.workspaces.delete(id);
    } catch {
      /* already gone */
    }
  }
  for (const id of ids.snapshots ?? []) {
    try {
      await aj.snapshots.delete(id);
    } catch {
      /* already gone */
    }
  }
}
