import type { ServiceId } from "../lib/api";
import { SERVICE_META } from "../lib/format";

/** Tiny tinted glyph used inside stacks (see <ServiceStack>). */
export function ServiceBadge({
  svc,
  size = 16,
}: {
  svc: ServiceId;
  size?: number;
}) {
  const meta = SERVICE_META[svc];
  return (
    <span
      className="rounded-full ring-1 ring-ink-900 grid place-items-center mono"
      style={{
        width: size,
        height: size,
        fontSize: Math.round(size * 0.55),
        background: `color-mix(in oklab, var(--color-${meta.accent}) 20%, var(--color-ink-850))`,
        color: `var(--color-${meta.accent})`,
      }}
      title={meta.label}
    >
      {meta.glyph}
    </span>
  );
}

/** Avatars-style overlap of service badges. */
export function ServiceStack({ services, size = 16 }: { services: ServiceId[]; size?: number }) {
  return (
    <div className="flex -space-x-1">
      {services.map((svc) => (
        <ServiceBadge key={svc} svc={svc} size={size} />
      ))}
    </div>
  );
}
