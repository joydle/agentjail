import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { useApi } from "../../lib/auth";
import type { Session, ServiceId } from "../../lib/api";
import { Panel, PanelHeader } from "../Panel";
import { Pill } from "../Pill";
import { Button } from "../Button";
import { Field } from "../Input";
import { ServicePicker } from "../ServicePicker";

export function MintForm({ available }: { available: Set<ServiceId> }) {
  const api = useApi();
  const qc = useQueryClient();
  const [picked, setPicked] = useState<Set<ServiceId>>(new Set());
  const [ttl, setTtl] = useState("600");
  const [created, setCreated] = useState<Session | null>(null);

  const mutate = useMutation({
    mutationFn: () =>
      api.sessions.create(Array.from(picked), ttl ? Number(ttl) : undefined),
    onSuccess: (s) => {
      setCreated(s);
      setPicked(new Set());
      qc.invalidateQueries({ queryKey: ["sessions"] });
    },
  });

  function toggle(svc: ServiceId) {
    const next = new Set(picked);
    next.has(svc) ? next.delete(svc) : next.add(svc);
    setPicked(next);
  }

  return (
    <Panel>
      <PanelHeader eyebrow="Mint" title="New session" />
      <div className="space-y-4">
        <div>
          <div className="text-[10px] uppercase tracking-[0.22em] text-ink-400 font-medium mb-2">
            Services
          </div>
          <ServicePicker
            mode="multi"
            selected={picked}
            onToggle={toggle}
            available={available}
          />
        </div>

        <Field
          label="TTL (seconds)"
          type="number"
          value={ttl}
          onChange={(e) => setTtl(e.target.value)}
          prefix="ttl"
        />

        <Button
          variant="primary"
          className="w-full justify-center"
          disabled={picked.size === 0 || mutate.isPending}
          onClick={() => mutate.mutate()}
        >
          {mutate.isPending ? "minting…" : `mint phantom session${picked.size > 1 ? "s" : ""}`}
        </Button>

        {mutate.error && (
          <div className="text-[11px] text-[var(--color-siren)]">
            {(mutate.error as Error).message}
          </div>
        )}

        {created && <MintedNote id={created.id} />}
      </div>
    </Panel>
  );
}

function MintedNote({ id }: { id: string }) {
  return (
    <div className="panel !bg-ink-950/60 !p-3 mt-2">
      <div className="flex items-center justify-between mb-2">
        <span className="text-[10px] uppercase tracking-[0.22em] text-ink-400">Minted</span>
        <Pill tone="phantom" dot>live</Pill>
      </div>
      <div className="mono text-[11px] text-ink-200 break-all">{id}</div>
      <div className="mt-2 text-[11px] text-ink-500">
        Expand the row on the left to view & copy env vars.
      </div>
    </div>
  );
}
