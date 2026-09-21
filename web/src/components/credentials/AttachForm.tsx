import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { useApi } from "../../lib/auth";
import type { ServiceId } from "../../lib/api";
import { Panel, PanelHeader } from "../Panel";
import { Button } from "../Button";
import { Field } from "../Input";
import { ServicePicker } from "../ServicePicker";

const PLACEHOLDERS: Record<ServiceId, string> = {
  openai:    "sk-…",
  anthropic: "sk-ant-…",
  github:    "ghp_…",
  stripe:    "sk_live_…",
};

export function AttachForm(
  { hasExisting, tenant }: { hasExisting: Set<ServiceId>; tenant?: string },
) {
  const api = useApi();
  const qc = useQueryClient();
  const [service, setService] = useState<ServiceId>("openai");
  const [secret, setSecret] = useState("");

  const put = useMutation({
    // `tenant` targets an explicit tenant — admins browsing another
    // tenant land here. Operators omit it (server uses their scope).
    mutationFn: () => api.credentials.put(service, secret, tenant),
    onSuccess: () => {
      setSecret("");
      qc.invalidateQueries({ queryKey: ["credentials", tenant ?? ""] });
    },
  });

  return (
    <Panel>
      <PanelHeader eyebrow="Attach" title="Add credential" />
      <div className="space-y-4">
        <div>
          <div className="text-[10px] uppercase tracking-[0.22em] text-ink-400 font-medium mb-2">
            Service
          </div>
          <ServicePicker mode="single" selected={service} onToggle={setService} />
        </div>

        <Field
          label="Secret"
          type="password"
          value={secret}
          onChange={(e) => setSecret(e.target.value)}
          placeholder={PLACEHOLDERS[service]}
          prefix="key"
          hint="encrypted at rest · only fingerprint is shown"
        />

        <Button
          variant="primary"
          className="w-full justify-center"
          disabled={!secret.trim() || put.isPending}
          onClick={() => put.mutate()}
        >
          {put.isPending ? "attaching…" : hasExisting.has(service) ? "rotate" : "attach"}
        </Button>

        {put.error && (
          <div className="text-[11px] text-[var(--color-siren)]">
            {(put.error as Error).message}
          </div>
        )}
        {put.isSuccess && (
          <div className="text-[11px] text-[var(--color-phantom)]">
            attached · fingerprint locked in vault
          </div>
        )}
      </div>
    </Panel>
  );
}
