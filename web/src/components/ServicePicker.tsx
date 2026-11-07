import type { ServiceId } from "../lib/api";
import { SERVICES, SERVICE_META } from "../lib/format";
import { cn } from "../lib/cn";

/**
 * Grid of four service buttons. Used for:
 *  - minting a session (multi-select, requires service to have a live key)
 *  - attaching a credential (single-select, all services always enabled)
 */
export function ServicePicker({
  mode,
  selected,
  onToggle,
  available,
}: {
  mode: "multi" | "single";
  /** for single: the lone chosen id; for multi: a Set of ids */
  selected: ServiceId | Set<ServiceId>;
  onToggle: (svc: ServiceId) => void;
  /** When provided, services outside the set are rendered disabled. */
  available?: Set<ServiceId>;
}) {
  function isOn(svc: ServiceId): boolean {
    return mode === "single"
      ? selected === svc
      : (selected as Set<ServiceId>).has(svc);
  }

  return (
    <div className="grid grid-cols-2 gap-2">
      {SERVICES.map((svc) => {
        const meta    = SERVICE_META[svc];
        const enabled = available ? available.has(svc) : true;
        const on      = isOn(svc);
        return (
          <button
            key={svc}
            type="button"
            disabled={!enabled}
            onClick={() => onToggle(svc)}
            className={cn(
              "flex items-center gap-2 h-10 px-3 rounded-lg ring-1 transition-all text-sm",
              !enabled && "opacity-50 cursor-not-allowed",
              on
                ? "ring-[var(--color-phantom)] bg-[var(--color-phantom-bg)] text-ink-100"
                : "ring-ink-700 bg-ink-900/50 text-ink-300 hover:ring-ink-600",
            )}
          >
            <span className="text-base" style={{ color: `var(--color-${meta.accent})` }}>
              {meta.glyph}
            </span>
            <span className="truncate">{meta.label}</span>
            {!enabled && <span className="ml-auto text-[10px] text-ink-500">∅</span>}
          </button>
        );
      })}
    </div>
  );
}
