import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { useApi } from "../lib/auth";
import { Panel, PanelHeader } from "../components/Panel";
import { Empty } from "../components/Empty";
import { SessionRow } from "../components/sessions/SessionRow";
import { MintForm } from "../components/sessions/MintForm";

/** Sessions page = list + mint form. */
export function Sessions() {
  const api = useApi();
  const qc = useQueryClient();
  const [openId, setOpenId] = useState<string | null>(null);

  const { data: sessions } = useQuery({
    queryKey: ["sessions"],
    queryFn: () => api.sessions.list(),
    refetchInterval: 4000,
  });
  const { data: creds } = useQuery({
    queryKey: ["credentials"],
    queryFn: () => api.credentials.list(),
  });

  const available = new Set((creds ?? []).map((c) => c.service));

  const close = useMutation({
    mutationFn: (id: string) => api.sessions.close(id),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["sessions"] }),
  });

  return (
    <div className="grid grid-cols-[1fr_420px] gap-4">
      <Panel padded={false}>
        <div className="px-5 py-4">
          <div className="flex items-center justify-between">
            <PanelHeader
              eyebrow="API Sessions"
              title={`${sessions?.length ?? 0} active`}
              className="!mb-0"
            />
            <div className="text-[11px] mono text-ink-500">auto-refreshing</div>
          </div>
          <p className="mt-1.5 text-[12px] text-ink-400 max-w-[520px]">
            A session bundles scoped{" "}
            <span className="mono text-ink-300">phm_…</span> tokens for a single agent run — short-lived, revocable, and only good for the services you pick.
          </p>
        </div>
        <div className="hairline" />
        {(sessions?.length ?? 0) === 0 ? (
          <Empty
            title="No active sessions"
            hint="Mint one on the right — you'll get a set of phantom env vars ready to drop into a sandbox."
          />
        ) : (
          (sessions ?? []).map((s) => (
            <SessionRow
              key={s.id}
              session={s}
              open={openId === s.id}
              onToggle={() => setOpenId(openId === s.id ? null : s.id)}
              onClose={() => close.mutate(s.id)}
              closing={close.isPending && close.variables === s.id}
            />
          ))
        )}
      </Panel>

      <MintForm available={available} />
    </div>
  );
}
