import type { ExecOptions } from "./types.js";

/**
 * Serialize an `ExecOptions` into the wire-shape the control plane expects.
 * Shared by `sessions.exec` and `runs.create` so the translation is defined
 * in exactly one place.
 */
export function encodeExecOptions(
  body: Record<string, unknown>,
  options: ExecOptions | undefined,
): void {
  if (!options) return;
  if (options.network    !== undefined) body.network     = options.network;
  if (options.seccomp    !== undefined) body.seccomp     = options.seccomp;
  if (options.cpuPercent !== undefined) body.cpu_percent = options.cpuPercent;
  if (options.maxPids    !== undefined) body.max_pids    = options.maxPids;
}
