import { cn } from "../lib/cn";

/** Inline "LABEL value" pair used in stat rows and terminal status bars. */
export function Stat({
  label,
  value,
  tone,
  className,
}: {
  label: string;
  value: string;
  tone?: "phantom" | "siren" | "flare";
  className?: string;
}) {
  return (
    <div className={cn("flex items-baseline gap-1.5", className)}>
      <span className="text-ink-500 uppercase tracking-[0.18em] text-[9.5px]">{label}</span>
      <span
        className="tabular-nums"
        style={{ color: tone ? `var(--color-${tone})` : "var(--color-ink-100)" }}
      >
        {value}
      </span>
    </div>
  );
}
