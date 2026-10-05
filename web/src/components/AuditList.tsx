import type { AuditRow } from "../lib/api";
import { Pill } from "./Pill";
import { clock, statusTone } from "../lib/format";

export function AuditList({
  rows,
  limit,
  showSession = true,
}: {
  rows: AuditRow[];
  limit?: number;
  showSession?: boolean;
}) {
  const data = limit ? rows.slice(0, limit) : rows;
  if (data.length === 0) {
    return (
      <div className="py-12 text-center text-xs text-ink-500 mono">
        no requests yet — mint a session and make one
      </div>
    );
  }
  return (
    <div className="divide-y divide-ink-800">
      {data.map((r) => (
        <div
          key={r.id}
          className="grid grid-cols-[64px_52px_1fr_auto] gap-3 items-center px-3 py-2 text-[12px] mono hover:bg-ink-850/50 transition-colors"
          style={{ animation: "ticker-in 0.35s ease both" }}
        >
          <span className="text-ink-500 tabular-nums">{clock(r.at)}</span>
          <Pill tone={statusTone(r.status)} className="!h-5 !px-1.5 !text-[10px]">
            {r.status || "—"}
          </Pill>
          <div className="flex items-center gap-2 min-w-0">
            <span className="text-ink-400 w-10 shrink-0">{r.method}</span>
            {r.service && (
              <span className="text-ink-300 shrink-0">{r.service}</span>
            )}
            <span className="text-ink-200 truncate">{r.path || "—"}</span>
            {r.reject_reason && (
              <span className="text-[var(--color-siren)] text-[11px] shrink-0">
                × {r.reject_reason}
              </span>
            )}
          </div>
          <div className="flex items-center gap-3 text-ink-500 text-[11px]">
            {r.upstream_ms !== null && <span className="tabular-nums">{r.upstream_ms}ms</span>}
            {showSession && r.session_id && (
              <span className="text-ink-600" title={r.session_id}>
                {r.session_id.slice(0, 6)}
              </span>
            )}
          </div>
        </div>
      ))}
    </div>
  );
}
