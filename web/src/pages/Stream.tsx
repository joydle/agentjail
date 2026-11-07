import { useQuery } from "@tanstack/react-query";
import { useMemo, useState } from "react";
import { useApi } from "../lib/auth";
import { Panel, PanelHeader } from "../components/Panel";
import { Pill } from "../components/Pill";
import { AuditList } from "../components/AuditList";

const FILTERS = ["all", "200", "4xx", "5xx", "blocked"] as const;
type Filter = (typeof FILTERS)[number];

const FILTER_LABELS: Record<Filter, string> = {
  all:       "show all requests",
  "200":     "show 2xx success responses",
  "4xx":     "show 4xx client errors",
  "5xx":     "show 5xx server errors",
  blocked:   "show requests blocked by scope / missing credential",
};

/**
 * Phantom-proxy audit feed.  Shows upstream API calls (OpenAI,
 * Anthropic, GitHub, Stripe) made by sandboxes — *not* every HTTP
 * request in the system.  Jail lifecycles live on the Jails page.
 */
export function Stream() {
  const api = useApi();
  const [filter, setFilter] = useState<Filter>("all");

  const { data } = useQuery({
    queryKey: ["audit", 200],
    queryFn: () => api.audit.recent(200),
    refetchInterval: 1500,
  });

  const filtered = useMemo(() => {
    const rows = data?.rows ?? [];
    switch (filter) {
      case "200":     return rows.filter((r) => r.status >= 200 && r.status < 300);
      case "4xx":     return rows.filter((r) => r.status >= 400 && r.status < 500);
      case "5xx":     return rows.filter((r) => r.status >= 500);
      case "blocked": return rows.filter((r) => r.reject_reason);
      default:        return rows;
    }
  }, [data, filter]);

  const counts = useMemo(() => {
    const r = data?.rows ?? [];
    return {
      all:     r.length,
      "200":   r.filter((x) => x.status >= 200 && x.status < 300).length,
      "4xx":   r.filter((x) => x.status >= 400 && x.status < 500).length,
      "5xx":   r.filter((x) => x.status >= 500).length,
      blocked: r.filter((x) => x.reject_reason).length,
    };
  }, [data]);

  const isEmpty = (data?.rows.length ?? 0) === 0;

  return (
    <Panel padded={false}>
      <div className="px-5 py-4 flex items-center justify-between">
        <PanelHeader
          eyebrow="Phantom proxy · audit"
          title="Upstream requests"
          className="!mb-0"
        />
        <div className="flex items-center gap-2">
          <Pill tone="phantom" dot>live</Pill>
          <span className="text-[11px] mono text-ink-500">
            {data?.total ?? 0} total · window 200
          </span>
        </div>
      </div>
      <div className="hairline" />
      <div className="px-5 py-3 flex items-center gap-1">
        {FILTERS.map((f) => (
          <button
            key={f}
            onClick={() => setFilter(f)}
            aria-label={FILTER_LABELS[f]}
            aria-pressed={filter === f}
            className={`h-7 px-3 rounded-full text-[11px] mono transition-colors ${
              filter === f
                ? "bg-ink-100 text-ink-950"
                : "text-ink-400 hover:text-ink-200"
            }`}
          >
            {f} · {counts[f]}
          </button>
        ))}
      </div>
      <div className="hairline" />
      {isEmpty ? (
        <EmptyAudit />
      ) : (
        <div className="max-h-[calc(100vh-260px)] overflow-y-auto">
          <AuditList rows={filtered} />
        </div>
      )}
    </Panel>
  );
}

/**
 * The audit table is phantom-proxy-only. "Silent" is expected until a
 * sandbox actually hits an upstream via `/v1/<service>/...`. Explain
 * that here so users don't think the page is broken.
 */
function EmptyAudit() {
  return (
    <div className="px-5 py-12 flex flex-col items-center text-center gap-3">
      <div className="text-[13px] text-ink-200 display font-semibold">
        No upstream traffic yet
      </div>
      <div className="max-w-md text-[12px] text-ink-400 leading-relaxed">
        This page shows requests that sandboxes make to real upstream
        services (<span className="mono text-ink-200">openai</span>,{" "}
        <span className="mono text-ink-200">anthropic</span>,{" "}
        <span className="mono text-ink-200">github</span>,{" "}
        <span className="mono text-ink-200">stripe</span>) through the
        phantom-token proxy. It doesn't show jail runs — those live on
        the <span className="text-ink-200">Jails</span> page.
      </div>
      <div className="text-[11px] text-ink-500 mono pt-1">
        to generate traffic: attach a credential, mint a session, hand{" "}
        <span className="text-ink-300">session.env</span> to a jail, call
        the upstream base URL it sees.
      </div>
    </div>
  );
}
