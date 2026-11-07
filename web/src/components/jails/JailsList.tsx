import type { JailRecord } from "../../lib/api";
import { Pill } from "../Pill";
import { clock, humanBytes, humanMs } from "../../lib/format";
import { cn } from "../../lib/cn";

const KIND_TONE: Record<JailRecord["kind"], "phantom" | "flare" | "iris" | "ink"> = {
  run:       "phantom",
  exec:      "iris",
  fork:      "flare",
  stream:    "phantom",
  workspace: "iris",
};

export function JailsList({
  rows,
  selected,
  onSelect,
}: {
  rows: JailRecord[];
  selected?: number;
  onSelect: (id: number) => void;
}) {
  if (rows.length === 0) {
    return (
      <div className="py-16 text-center text-xs text-ink-500 mono">
        no jails yet — launch something from the playground
      </div>
    );
  }
  return (
    <div className="divide-y divide-ink-800">
      {rows.map((r) => (
        <Row key={r.id} rec={r} on={selected === r.id} onSelect={onSelect} />
      ))}
    </div>
  );
}

function Row({
  rec,
  on,
  onSelect,
}: {
  rec: JailRecord;
  on: boolean;
  onSelect: (id: number) => void;
}) {
  return (
    <button
      onClick={() => onSelect(rec.id)}
      className={cn(
        "w-full grid grid-cols-[56px_90px_1fr_100px_96px_auto] gap-3 items-center px-4 py-2.5 text-[12px] text-left hover:bg-ink-850/50 transition-colors",
        on && "bg-ink-850/70",
      )}
    >
      <span className="text-ink-500 mono tabular-nums">{clock(rec.started_at)}</span>
      <Pill tone={KIND_TONE[rec.kind]}>{rec.kind}</Pill>
      <span className="mono text-ink-200 truncate">{rec.label}</span>
      <StatusPill rec={rec} />
      <span className="text-ink-400 mono tabular-nums justify-self-end">
        {rec.duration_ms != null ? humanMs(rec.duration_ms) : "—"}
      </span>
      <span className="text-ink-500 mono tabular-nums justify-self-end">
        {rec.memory_peak_bytes != null ? humanBytes(rec.memory_peak_bytes) : "—"}
      </span>
    </button>
  );
}

function StatusPill({ rec }: { rec: JailRecord }) {
  if (rec.status === "running") return <Pill tone="iris" dot>running</Pill>;
  if (rec.status === "error")   return <Pill tone="siren">error</Pill>;
  if (rec.timed_out)            return <Pill tone="siren">timeout</Pill>;
  if (rec.oom_killed)           return <Pill tone="siren">oom</Pill>;
  if (rec.exit_code === 0)      return <Pill tone="phantom">ok</Pill>;
  return <Pill tone="siren">exit {rec.exit_code}</Pill>;
}
