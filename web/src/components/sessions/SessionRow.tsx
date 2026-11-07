import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import type { Session } from "../../lib/api";
import { useApi } from "../../lib/auth";
import { Button } from "../Button";
import { ServiceStack } from "../ServiceBadge";
import { AuditList } from "../AuditList";
import { timeAgo } from "../../lib/format";
import { cn } from "../../lib/cn";

type Tab = "env" | "traffic";

export function SessionRow({
  session,
  open,
  onToggle,
  onClose,
  closing,
}: {
  session: Session;
  open: boolean;
  onToggle: () => void;
  onClose: () => void;
  closing: boolean;
}) {
  const [tab, setTab] = useState<Tab>("env");

  return (
    <div className="border-b border-ink-800 last:border-b-0">
      <button
        onClick={onToggle}
        className="w-full grid grid-cols-[auto_1fr_auto_auto] gap-4 items-center px-5 py-3 text-left hover:bg-ink-850/40 transition-colors"
      >
        <span className="w-1.5 h-6 rounded-full bg-[var(--color-phantom)] opacity-80" />
        <div className="min-w-0">
          <div className="mono text-[13px] text-ink-100 truncate">{session.id}</div>
          <div className="text-[11px] text-ink-500 mt-0.5">
            created {timeAgo(session.created_at)}
            {session.expires_at
              ? ` · expires ${timeAgo(session.expires_at)}`
              : " · no expiry"}
          </div>
        </div>
        <ServiceStack services={session.services} size={20} />
        <span
          className={cn("text-ink-500 text-sm transition-transform", open && "rotate-180")}
        >
          ⌄
        </span>
      </button>
      {open && (
        <div className="px-5 pb-5 space-y-3">
          <Tabs tab={tab} setTab={setTab} />
          {tab === "env" ? (
            <EnvTable env={session.env} />
          ) : (
            <SessionTraffic sessionId={session.id} />
          )}
          <div className="flex justify-end">
            <Button variant="danger" size="sm" onClick={onClose} disabled={closing}>
              {closing ? "revoking…" : "revoke session"}
            </Button>
          </div>
        </div>
      )}
    </div>
  );
}

function Tabs({ tab, setTab }: { tab: Tab; setTab: (t: Tab) => void }) {
  return (
    <div className="flex items-center gap-1 rounded-full ring-1 ring-ink-800 p-0.5 w-fit">
      <TabBtn on={tab === "env"}     onClick={() => setTab("env")}>env vars</TabBtn>
      <TabBtn on={tab === "traffic"} onClick={() => setTab("traffic")}>traffic</TabBtn>
    </div>
  );
}

function TabBtn({ on, onClick, children }: { on: boolean; onClick: () => void; children: React.ReactNode }) {
  return (
    <button
      onClick={onClick}
      className={cn(
        "h-6 px-3 rounded-full text-[11px] mono transition-colors",
        on ? "bg-ink-100 text-ink-950" : "text-ink-400 hover:text-ink-200",
      )}
    >
      {children}
    </button>
  );
}

function SessionTraffic({ sessionId }: { sessionId: string }) {
  const api = useApi();
  const { data } = useQuery({
    queryKey: ["audit", 200],
    queryFn:  () => api.audit.recent(200),
    refetchInterval: 2000,
  });

  const rows = (data?.rows ?? []).filter((r) => r.session_id === sessionId);

  return (
    <div className="panel !bg-ink-950/60 overflow-hidden">
      {rows.length === 0 ? (
        <div className="py-6 text-center text-xs text-ink-500 mono">
          no traffic on this session yet
        </div>
      ) : (
        <div className="max-h-[280px] overflow-y-auto">
          <AuditList rows={rows} showSession={false} />
        </div>
      )}
    </div>
  );
}

function EnvTable({ env }: { env: Record<string, string> }) {
  return (
    <div className="panel !bg-ink-950/60 p-4 space-y-2">
      {Object.entries(env).map(([k, v]) => (
        <div
          key={k}
          className="grid grid-cols-[180px_1fr_auto] gap-3 items-center text-[12px] mono"
        >
          <span className="text-ink-400">{k}</span>
          <span className="text-ink-200 truncate">{v}</span>
          <button
            className="text-ink-500 hover:text-ink-200 text-[10px] uppercase tracking-widest"
            onClick={() => navigator.clipboard.writeText(v)}
          >
            copy
          </button>
        </div>
      ))}
    </div>
  );
}
