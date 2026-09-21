import { useQuery } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import type { AuditRow, JailRecord } from "../../lib/api";
import { useApi } from "../../lib/auth";
import { Panel, PanelHeader } from "../Panel";
import { Pill } from "../Pill";
import { Sparkline } from "../Sparkline";
import { OutputBlock } from "../OutputBlock";
import { AuditList } from "../AuditList";
import { ForkGraph } from "./ForkGraph";
import { humanBytes, humanMs, statusTone, timeAgo } from "../../lib/format";

/**
 * Jail detail pane — resource bar, live sparklines while running,
 * network-activity section (audit rows filtered by session), stdout/stderr.
 */
export function JailDetail({
  rec,
  onSelect,
}: {
  rec: JailRecord | null;
  onSelect?: (id: number) => void;
}) {
  if (!rec) {
    return (
      <Panel>
        <div className="py-10 text-center text-xs text-ink-500 mono">
          select a jail to inspect its output, stats, and network activity
        </div>
      </Panel>
    );
  }

  return (
    <div className="space-y-4 h-full flex flex-col">
      <Panel padded={false} className="flex-1 min-h-0 flex flex-col">
        <div className="px-5 py-3 flex items-start justify-between gap-3">
          <PanelHeader
            eyebrow={`${rec.kind} · jail #${rec.id}`}
            title={rec.label}
            className="!mb-0"
          />
          <StatusPill rec={rec} />
        </div>
        <div className="hairline" />

        <LiveStats rec={rec} />

        {rec.session_id && (
          <div className="px-5 py-2 text-[11px] mono text-ink-500 border-b border-ink-800">
            session <span className="text-ink-200">{rec.session_id}</span>
          </div>
        )}

        <div className="p-5 space-y-4 flex-1 min-h-0 overflow-y-auto">
          {rec.error && <OutputBlock label="error"  tone="siren"   text={rec.error}  showSize={false} />}
          {rec.stdout && <OutputBlock label="stdout" tone="phantom" text={rec.stdout} />}
          {rec.stderr && <OutputBlock label="stderr" tone="flare"   text={rec.stderr} />}
          {!rec.stdout && !rec.stderr && !rec.error && rec.status === "completed" && (
            <div className="text-[12px] mono text-ink-500">
              program produced no output · exit {rec.exit_code}
            </div>
          )}
          {rec.status === "running" && !rec.stdout && !rec.stderr && (
            <div className="text-[12px] mono text-ink-500">
              <span className="text-[var(--color-phantom)]">●</span> running —
              stdout/stderr will populate on completion (use <span className="text-ink-300">/v1/runs/stream</span> for live lines)
            </div>
          )}
        </div>
      </Panel>

      <ConfigPanel config={rec.config ?? null} />
      {rec.kind === "fork" && <ForkGraph rec={rec} onSelect={onSelect} />}
      {rec.session_id && <NetworkPanel sessionId={rec.session_id} />}
    </div>
  );
}

// ─── jail configuration ─────────────────────────────────────────────────

function ConfigPanel({ config }: { config: JailRecord["config"] | null }) {
  return (
    <Panel padded={false}>
      <div className="px-5 py-3">
        <PanelHeader eyebrow="Configuration" title="Jail settings" className="!mb-0" />
      </div>
      <div className="hairline" />
      {config ? <ConfigBody config={config} /> : (
        <div className="px-5 py-4 text-[12px] mono text-ink-500">
          not captured — run predates the <span className="text-ink-300">config_json</span> column
        </div>
      )}
    </Panel>
  );
}

function ConfigBody({ config }: { config: NonNullable<JailRecord["config"]> }) {
  const networkDetail =
    config.network_mode === "allowlist" && config.network_domains?.length
      ? `allowlist · ${config.network_domains.length}`
      : config.network_mode;
  return (
    <>
      <div className="px-5 py-4 grid grid-cols-2 gap-x-6 gap-y-2 text-[12px]">
        <ConfigRow label="Network"  value={networkDetail} />
        <ConfigRow label="Seccomp"  value={config.seccomp} />
        <ConfigRow label="Memory"   value={`${config.memory_mb} MB`} />
        <ConfigRow label="Timeout"  value={`${config.timeout_secs} s`} />
        <ConfigRow label="CPU"      value={`${config.cpu_percent}%`} />
        <ConfigRow label="Max PIDs" value={String(config.max_pids)} />
        {config.git_repo && (
          <ConfigRow
            label="Git"
            value={`${config.git_repo}${config.git_ref ? ` @ ${config.git_ref}` : ""}`}
            span
          />
        )}
      </div>
      {config.network_mode === "allowlist" && config.network_domains?.length ? (
        <div className="px-5 pb-4">
          <div className="text-[10px] font-medium uppercase tracking-[0.2em] text-ink-400 mb-2">
            Allowlist
          </div>
          <div className="flex flex-wrap gap-1.5">
            {config.network_domains.map((d) => (
              <span
                key={d}
                className="mono text-[11px] text-phantom bg-ink-800/60 rounded px-2 py-0.5"
              >
                {d}
              </span>
            ))}
          </div>
        </div>
      ) : null}
    </>
  );
}

function ConfigRow({ label, value, span }: { label: string; value: string; span?: boolean }) {
  return (
    <div className={span ? "col-span-2 grid grid-cols-[80px_1fr] gap-3 items-baseline" : "grid grid-cols-[80px_1fr] gap-3 items-baseline"}>
      <span className="text-ink-500">{label}</span>
      <span className="mono text-ink-100 break-all">{value}</span>
    </div>
  );
}

// ─── live stats ──────────────────────────────────────────────────────────

/**
 * Tracks the last N samples of memory / cpu / io as we poll the jail row.
 * When the jail completes, the series freezes at its final values.
 */
function useSample(rec: JailRecord) {
  const [mem, setMem] = useState<number[]>([]);
  const [cpu, setCpu] = useState<number[]>([]);
  const [ior, setIor] = useState<number[]>([]);
  const [iow, setIow] = useState<number[]>([]);

  useEffect(() => {
    if (rec.status !== "running") return;
    setMem((s) => pushCap(s, rec.memory_peak_bytes ?? 0));
    setCpu((s) => pushCap(s, rec.cpu_usage_usec ?? 0));
    setIor((s) => pushCap(s, rec.io_read_bytes ?? 0));
    setIow((s) => pushCap(s, rec.io_write_bytes ?? 0));
  }, [rec.memory_peak_bytes, rec.cpu_usage_usec, rec.io_read_bytes, rec.io_write_bytes, rec.status]);

  useEffect(() => {
    setMem([]); setCpu([]); setIor([]); setIow([]);
  }, [rec.id]);

  return { mem, cpu, ior, iow };
}

function pushCap(arr: number[], v: number, max = 60): number[] {
  const n = [...arr, v];
  return n.length > max ? n.slice(-max) : n;
}

function LiveStats({ rec }: { rec: JailRecord }) {
  const { mem, cpu, ior, iow } = useSample(rec);
  const isLive = rec.status === "running";

  return (
    <div className="px-5 py-3 border-b border-ink-800 bg-ink-900/40">
      <div className="flex items-center gap-2 mb-2">
        <span className="text-[9.5px] uppercase tracking-[0.2em] text-ink-500">live stats</span>
        {isLive ? <Pill tone="iris" dot>sampling @ 500ms</Pill> : <Pill tone="ink">frozen</Pill>}
        <span className="text-[10.5px] mono text-ink-600 ml-auto">
          started {timeAgo(rec.started_at)}
          {rec.duration_ms ? ` · ran ${humanMs(rec.duration_ms)}` : ""}
        </span>
      </div>
      <div className="grid grid-cols-2 gap-3">
        <StatCell
          label="memory"
          value={rec.memory_peak_bytes != null ? humanBytes(rec.memory_peak_bytes) : "—"}
          series={mem}
          tone="#7FFFBD"
        />
        <StatCell
          label="cpu"
          value={rec.cpu_usage_usec != null ? humanMs(Math.round(rec.cpu_usage_usec / 1000)) : "—"}
          series={cpu}
          tone="#FFB366"
        />
        <StatCell
          label="io read"
          value={rec.io_read_bytes != null ? humanBytes(rec.io_read_bytes) : "—"}
          series={ior}
          tone="#9B8CFF"
        />
        <StatCell
          label="io write"
          value={rec.io_write_bytes != null ? humanBytes(rec.io_write_bytes) : "—"}
          series={iow}
          tone="#FF6B80"
        />
      </div>
    </div>
  );
}

function StatCell({
  label,
  value,
  series,
  tone,
}: {
  label: string;
  value: string;
  series: number[];
  tone: string;
}) {
  return (
    <div className="relative panel !p-2.5 overflow-hidden">
      <div className="flex items-baseline justify-between">
        <span className="text-[9.5px] uppercase tracking-[0.2em] text-ink-500">{label}</span>
        <span className="mono text-[12.5px] text-ink-100 tabular-nums">{value}</span>
      </div>
      <div className="mt-1 h-6 -mx-0.5">
        {series.length > 1 ? (
          <Sparkline data={series} stroke={tone} height={24} />
        ) : (
          <div className="h-6 mono text-[10px] text-ink-600 grid place-items-end pr-1">
            {series.length === 0 ? "waiting…" : "…"}
          </div>
        )}
      </div>
    </div>
  );
}

// ─── network panel ───────────────────────────────────────────────────────

function NetworkPanel({ sessionId }: { sessionId: string }) {
  const api = useApi();
  const { data } = useQuery({
    queryKey: ["audit", 300, "network", sessionId],
    queryFn: () => api.audit.recent(300),
    refetchInterval: 2500,
  });
  const rows: AuditRow[] = (data?.rows ?? []).filter((r) => r.session_id === sessionId);

  const stats = rows.reduce(
    (acc, r) => {
      acc.total += 1;
      if (r.status >= 200 && r.status < 300) acc.ok += 1;
      else if (r.reject_reason) acc.blocked += 1;
      else acc.fail += 1;
      return acc;
    },
    { total: 0, ok: 0, blocked: 0, fail: 0 },
  );

  return (
    <Panel padded={false}>
      <div className="px-5 py-3 flex items-center justify-between">
        <PanelHeader eyebrow="Network" title="Phantom traffic" className="!mb-0" />
        <div className="flex items-center gap-2 text-[11px] mono text-ink-500">
          <Pill tone={stats.total ? "phantom" : "ink"} dot={stats.total > 0}>
            {stats.total} req
          </Pill>
          {stats.ok > 0      && <span className="text-[var(--color-phantom)]">{stats.ok} ok</span>}
          {stats.fail > 0    && <span className={toneClass(statusTone(500))}>{stats.fail} fail</span>}
          {stats.blocked > 0 && <span className="text-[var(--color-siren)]">{stats.blocked} blocked</span>}
        </div>
      </div>
      <div className="hairline" />
      <div className="max-h-[240px] overflow-y-auto">
        <AuditList rows={rows} limit={50} showSession={false} />
      </div>
    </Panel>
  );
}

function toneClass(t: ReturnType<typeof statusTone>) {
  switch (t) {
    case "phantom": return "text-[var(--color-phantom)]";
    case "flare":   return "text-[var(--color-flare)]";
    case "siren":   return "text-[var(--color-siren)]";
    default:        return "text-ink-400";
  }
}

function StatusPill({ rec }: { rec: JailRecord }) {
  if (rec.status === "running") return <Pill tone="iris" dot>running</Pill>;
  if (rec.status === "error")   return <Pill tone="siren">error</Pill>;
  if (rec.timed_out)            return <Pill tone="siren">timeout</Pill>;
  if (rec.oom_killed)           return <Pill tone="siren">oom-killed</Pill>;
  if (rec.exit_code === 0)      return <Pill tone="phantom">ok</Pill>;
  return <Pill tone="siren">exit {rec.exit_code}</Pill>;
}

