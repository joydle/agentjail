import { Link } from "react-router-dom";
import type { CredentialRecord, ServiceId } from "../../lib/api";
import { Panel, PanelHeader } from "../Panel";
import { Pill } from "../Pill";
import { Button } from "../Button";
import { SERVICES, SERVICE_META } from "../../lib/format";

export function VaultPanel({ creds }: { creds?: CredentialRecord[] }) {
  const byService = new Map((creds ?? []).map((c) => [c.service, c] as const));
  return (
    <Panel>
      <PanelHeader
        eyebrow="Vault"
        title="Upstream credentials"
        action={
          <Link to="/integrations">
            <Button variant="ghost" size="sm">manage →</Button>
          </Link>
        }
      />
      <ul className="space-y-2">
        {SERVICES.map((svc) => (
          <VaultRow key={svc} svc={svc} rec={byService.get(svc)} />
        ))}
      </ul>
    </Panel>
  );
}

function VaultRow({ svc, rec }: { svc: ServiceId; rec?: CredentialRecord }) {
  const meta = SERVICE_META[svc];
  return (
    <li className="flex items-center justify-between gap-3 h-10 px-3 rounded-lg ring-1 ring-ink-800 bg-ink-900/50">
      <div className="flex items-center gap-3 min-w-0">
        <span className="text-lg leading-none" style={{ color: `var(--color-${meta.accent})` }}>
          {meta.glyph}
        </span>
        <span className="text-sm text-ink-200">{meta.label}</span>
      </div>
      {rec ? (
        <div className="flex items-center gap-2">
          <span className="mono text-[10px] text-ink-500">{rec.fingerprint}</span>
          <Pill tone="phantom" dot>live</Pill>
        </div>
      ) : (
        <Pill tone="ink">not set</Pill>
      )}
    </li>
  );
}
