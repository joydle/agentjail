import { Sparkline } from "./Sparkline";
import { cn } from "../lib/cn";

interface MetricProps {
  label: string;
  value: string | number;
  delta?: string;
  tone?: "phantom" | "flare" | "iris";
  series?: number[];
  className?: string;
}

const toneHex: Record<NonNullable<MetricProps["tone"]>, string> = {
  phantom: "#7FFFBD",
  flare:   "#FFB366",
  iris:    "#9B8CFF",
};

export function Metric({ label, value, delta, tone = "phantom", series, className }: MetricProps) {
  return (
    <div className={cn("panel p-4 relative overflow-hidden group", className)}>
      <div className="flex items-start justify-between">
        <div className="text-[10px] uppercase tracking-[0.22em] text-ink-400 font-medium">{label}</div>
        {delta && (
          <span className="text-[10px] mono text-ink-400">{delta}</span>
        )}
      </div>
      <div className="mt-2 flex items-baseline gap-1.5">
        <span className="display text-3xl font-semibold text-ink-100 tabular-nums">
          {value}
        </span>
      </div>
      {series && (
        <div className="mt-2 -mx-1">
          <Sparkline data={series} stroke={toneHex[tone]} height={28} />
        </div>
      )}
      <div
        className="absolute inset-x-0 -bottom-px h-px opacity-0 group-hover:opacity-100 transition-opacity"
        style={{
          background: `linear-gradient(90deg, transparent, ${toneHex[tone]}, transparent)`,
        }}
      />
    </div>
  );
}
