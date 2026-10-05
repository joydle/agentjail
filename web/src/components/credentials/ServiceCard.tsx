import type { CredentialRecord, ServiceId } from "../../lib/api";
import { Pill } from "../Pill";
import { Button } from "../Button";
import { SERVICE_META, timeAgo } from "../../lib/format";

export function ServiceCard({
  svc,
  rec,
  onDelete,
  deleting,
}: {
  svc: ServiceId;
  rec?: CredentialRecord;
  onDelete: () => void;
  deleting: boolean;
}) {
  const meta = SERVICE_META[svc];
  return (
    <div
      className="panel !p-4 relative overflow-hidden"
      style={{
        background: rec
          ? `linear-gradient(180deg, color-mix(in oklab, var(--color-${meta.accent}) 4%, var(--color-ink-850)), var(--color-ink-900))`
          : undefined,
      }}
    >
      <header className="flex items-start justify-between">
        <div className="flex items-center gap-2.5">
          <span
            className="text-2xl leading-none"
            style={{ color: `var(--color-${meta.accent})` }}
          >
            {meta.glyph}
          </span>
          <div>
            <div className="text-sm font-medium text-ink-100">{meta.label}</div>
            <div className="text-[11px] text-ink-500 mono">{svc}</div>
          </div>
        </div>
        {rec ? <Pill tone="phantom" dot>live</Pill> : <Pill>∅ empty</Pill>}
      </header>

      <Facts rec={rec} serviceLabel={meta.label} />

      {rec && (
        <div className="mt-3 flex justify-end">
          <Button variant="danger" size="sm" onClick={onDelete} disabled={deleting}>
            {deleting ? "removing…" : "remove"}
          </Button>
        </div>
      )}
    </div>
  );
}

function Facts({
  rec,
  serviceLabel,
}: {
  rec?: CredentialRecord;
  serviceLabel: string;
}) {
  if (!rec) {
    return (
      <div className="mt-4 text-[11px] mono min-h-[54px] text-ink-500">
        no key attached — sessions for {serviceLabel} disabled.
      </div>
    );
  }
  return (
    <dl className="mt-4 space-y-1 text-[11px] mono min-h-[54px]">
      <Row term="fp"      value={rec.fingerprint} />
      <Row term="added"   value={timeAgo(rec.added_at)} />
      <Row term="rotated" value={timeAgo(rec.updated_at)} />
    </dl>
  );
}

function Row({ term, value }: { term: string; value: string }) {
  return (
    <div className="flex items-center justify-between">
      <dt className="text-ink-500">{term}</dt>
      <dd className="text-ink-200">{value}</dd>
    </div>
  );
}
