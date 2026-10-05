import { useQuery } from "@tanstack/react-query";
import { useApi } from "../lib/auth";
import { Hero } from "../components/overview/Hero";
import { MetricGrid } from "../components/overview/MetricGrid";
import { StreamBlock } from "../components/overview/StreamBlock";
import { VaultPanel } from "../components/overview/VaultPanel";
import { ActiveSessionsPanel } from "../components/overview/ActiveSessionsPanel";

/** Dashboard = task-first hero + 3 metrics + stream | vault + active sessions. */
export function Overview() {
  const api = useApi();

  const { data: stats      } = useQuery({ queryKey: ["stats"],       queryFn: api.stats,              refetchInterval: 2000  });
  const { data: sessions   } = useQuery({ queryKey: ["sessions"],    queryFn: api.sessions.list,      refetchInterval: 4000  });
  const { data: creds      } = useQuery({ queryKey: ["credentials"], queryFn: api.credentials.list,   refetchInterval: 10000 });
  const { data: audit      } = useQuery({ queryKey: ["audit", 20],   queryFn: () => api.audit.recent(20), refetchInterval: 2000 });
  const { data: workspaces } = useQuery({ queryKey: ["workspaces", { limit: 1 }], queryFn: () => api.workspaces.list({ limit: 1 }), refetchInterval: 10000 });

  // Bead rate derives from observed successful traffic — never synthetic.
  const recentOk = audit?.rows.filter((r) => r.status >= 200 && r.status < 300).length ?? 0;
  const rate = Math.max(0, Math.min(6, Math.round(recentOk / 3)));

  const hasCreds    = (creds?.length ?? 0) > 0;
  const hasSessions = (sessions?.length ?? 0) > 0;
  const hasProjects = (workspaces?.total ?? 0) > 0;

  return (
    <div className="grid gap-4 grid-cols-[minmax(0,1fr)_380px]">
      <div className="space-y-4 min-w-0">
        <Hero
          totalEvents={audit?.total ?? 0}
          rate={rate}
          hasCreds={hasCreds}
          hasSessions={hasSessions}
          hasProjects={hasProjects}
        />
        <MetricGrid
          sessions={sessions?.length ?? 0}
          active={stats?.active_execs ?? 0}
          totalExecs={stats?.total_execs ?? 0}
          proxyEvents={audit?.total ?? 0}
        />
        <StreamBlock rows={audit?.rows ?? []} />
      </div>
      <div className="space-y-4">
        <VaultPanel creds={creds} />
        <ActiveSessionsPanel sessions={sessions} />
      </div>
    </div>
  );
}
