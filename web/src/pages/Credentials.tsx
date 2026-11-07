import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useParams } from "react-router-dom";
import { useApi, useWhoami } from "../lib/auth";
import type { ServiceId } from "../lib/api";
import { Panel, PanelHeader } from "../components/Panel";
import { Pill } from "../components/Pill";
import { ServiceCard } from "../components/credentials/ServiceCard";
import { AttachForm } from "../components/credentials/AttachForm";
import { SERVICES } from "../lib/format";

/** Integrations page = grid of service cards + attach form. */
export function Credentials() {
  const api = useApi();
  const qc = useQueryClient();
  const me = useWhoami();

  // The page lives under `/t/:tenant/integrations`, so the URL tells
  // us which tenant's credentials to show. Admins can change the URL
  // to browse another tenant; operators are locked server-side even if
  // they do (the API returns their own scope).
  const { tenant: urlTenant } = useParams<{ tenant: string }>();
  const viewing = urlTenant ?? me?.tenant ?? "";

  const { data: creds } = useQuery({
    queryKey: ["credentials", viewing],
    queryFn:  () => api.credentials.list(viewing || undefined),
    enabled:  !!viewing,
  });

  const del = useMutation({
    mutationFn: (svc: ServiceId) => api.credentials.delete(svc, viewing),
    onSuccess:  () => qc.invalidateQueries({ queryKey: ["credentials", viewing] }),
  });

  const byService = new Map((creds ?? []).map((c) => [c.service, c] as const));
  const existing = new Set(byService.keys());

  const count = creds?.length ?? 0;
  const tone = count === 0 ? "ink" : count === SERVICES.length ? "phantom" : "flare";
  const crossTenant = !!(me && viewing && viewing !== me.tenant);

  return (
    <div className="grid grid-cols-[1fr_420px] gap-4">
      <Panel padded={false}>
        <div className="px-5 py-4">
          <PanelHeader
            eyebrow="Integrations"
            title="Connected services"
            action={
              <Pill tone={tone} dot={count > 0}>
                {count} of {SERVICES.length} connected
              </Pill>
            }
            className="!mb-0"
          />
          <p className="mt-1.5 text-[12px] text-ink-400 max-w-[520px]">
            Attach your real API keys here — sandboxes only ever see{" "}
            <span className="mono text-ink-300">phm_…</span> tokens that the proxy swaps back at request time.
          </p>
          <div className="mt-2 flex items-center gap-2 text-[11px] mono">
            <span className="text-ink-500">tenant</span>
            <span className="text-ink-100">{viewing || "…"}</span>
            {crossTenant && (
              <span className="text-[var(--color-phantom)] uppercase tracking-wider text-[9.5px] px-1.5 py-0.5 rounded ring-1 ring-[var(--color-phantom)]/40 bg-[var(--color-phantom)]/10">
                cross-tenant view
              </span>
            )}
          </div>
        </div>
        <div className="hairline" />
        <div className="p-5 grid grid-cols-2 gap-3">
          {SERVICES.map((svc) => (
            <ServiceCard
              key={svc}
              svc={svc}
              rec={byService.get(svc)}
              onDelete={() => del.mutate(svc)}
              deleting={del.isPending && del.variables === svc}
            />
          ))}
        </div>
      </Panel>

      <AttachForm hasExisting={existing} tenant={viewing} />
    </div>
  );
}
