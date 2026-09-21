import type { ExecResult } from "../../lib/api";
import { Panel } from "../Panel";
import { Pill } from "../Pill";
import { Stat } from "../Stat";
import { OutputBlock } from "../OutputBlock";
import { humanBytes, humanMs } from "../../lib/format";
import { cn } from "../../lib/cn";
import type { Recipe } from "../../lib/recipes";

export function Terminal({
  open,
  onToggle,
  recipe,
  result,
  error,
  pending,
}: {
  open: boolean;
  onToggle: () => void;
  recipe: Recipe;
  result?: ExecResult;
  error: unknown;
  pending: boolean;
}) {
  const runnable = recipe.kind === "run";
  const statusText = !runnable
    ? "(not executable — snippet)"
    : pending
    ? "spinning up sandbox…"
    : result
    ? `exit ${result.exit_code} · ${humanMs(result.duration_ms)}`
    : error
    ? "error"
    : "ready";

  return (
    <Panel padded={false} frosted className="flex flex-col overflow-hidden">
      <button
        onClick={onToggle}
        className="w-full flex items-center gap-3 px-5 h-11 border-b border-ink-800 text-left hover:bg-ink-850/40 transition-colors"
      >
        <span className={cn("text-ink-500 text-xs transition-transform", open && "rotate-90")}>▸</span>
        <span className="text-[10px] uppercase tracking-[0.22em] text-ink-400 font-medium">
          Terminal
        </span>
        <span className="text-ink-600">·</span>
        <span className="text-[11px] mono text-ink-500 truncate">{statusText}</span>
        {result && <ExitPill result={result} className="ml-auto" />}
        {pending && (
          <span className="ml-auto inline-flex items-center gap-1.5 text-[11px] mono text-[var(--color-phantom)]">
            <span
              className="w-1.5 h-1.5 rounded-full bg-[var(--color-phantom)]"
              style={{ animation: "pulse-ring 1.5s ease infinite" }}
            />
            running
          </span>
        )}
      </button>

      {open && (
        <div className="flex flex-col min-h-0 max-h-[46vh]">
          {result && <StatsBar result={result} />}
          <div className="flex-1 min-h-0 overflow-y-auto px-5 py-4 space-y-4 bg-ink-950/60">
            <Body recipe={recipe} result={result} pending={pending} error={error} />
          </div>
        </div>
      )}
    </Panel>
  );
}

function Body({
  recipe,
  result,
  pending,
  error,
}: {
  recipe: Recipe;
  result?: ExecResult;
  pending: boolean;
  error: unknown;
}) {
  const runnable = recipe.kind === "run";
  if (!runnable) {
    return (
      <div className="text-[12px] text-ink-400 leading-relaxed">
        This {recipe.group === "sdk" ? "SDK" : "preset"} snippet isn&rsquo;t executed in the
        browser — copy it into your own project.
      </div>
    );
  }
  if (pending) {
    return (
      <div className="py-8 text-center text-xs text-ink-500 mono">
        <span className="text-[var(--color-phantom)]">●</span> spinning up sandbox — cgroups,
        seccomp, namespaces…
      </div>
    );
  }
  if (error instanceof Error) {
    return <OutputBlock label="error" tone="siren" text={error.message} showSize={false} />;
  }
  if (!result) {
    return (
      <div className="py-8 text-center text-xs text-ink-500 mono">
        press <span className="text-ink-300">run</span> to execute in a fresh jail
      </div>
    );
  }
  return (
    <>
      {result.stdout && <OutputBlock label="stdout" tone="phantom" text={result.stdout} />}
      {result.stderr && <OutputBlock label="stderr" tone="flare" text={result.stderr} />}
      {!result.stdout && !result.stderr && (
        <div className="py-8 text-center text-xs text-ink-500 mono">
          program produced no output · exit {result.exit_code}
        </div>
      )}
    </>
  );
}

function ExitPill({ result, className }: { result: ExecResult; className?: string }) {
  if (result.timed_out)  return <Pill tone="siren" className={className}>timeout</Pill>;
  if (result.oom_killed) return <Pill tone="siren" className={className}>oom-killed</Pill>;
  return (
    <Pill tone={result.exit_code === 0 ? "phantom" : "siren"} className={className}>
      exit {result.exit_code}
    </Pill>
  );
}

function StatsBar({ result }: { result: ExecResult }) {
  const kills = result.timed_out || result.oom_killed;
  return (
    <div className="flex items-center gap-5 px-5 h-9 border-b border-ink-800 bg-ink-900/40 text-[11px] mono">
      <Stat label="time"   value={humanMs(result.duration_ms)} />
      <Stat label="memory" value={result.stats ? humanBytes(result.stats.memory_peak_bytes) : "—"} />
      <Stat label="cpu"    value={result.stats ? humanMs(Math.round(result.stats.cpu_usage_usec / 1000)) : "—"} />
      <Stat
        label="io"
        value={
          result.stats
            ? `${humanBytes(result.stats.io_read_bytes)} r · ${humanBytes(result.stats.io_write_bytes)} w`
            : "—"
        }
      />
      <div className="ml-auto">
        {kills ? (
          <Pill tone="siren">{result.timed_out ? "timeout" : "oom-killed"}</Pill>
        ) : (
          <span className="text-ink-500">sandboxed · network none · seccomp standard</span>
        )}
      </div>
    </div>
  );
}
