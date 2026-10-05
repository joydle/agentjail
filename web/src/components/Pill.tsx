import type { ReactNode } from "react";
import { cn } from "../lib/cn";

type Tone = "phantom" | "flare" | "siren" | "iris" | "ink";

const toneClass: Record<Tone, string> = {
  phantom: "text-phantom bg-[var(--color-phantom-bg)] ring-[var(--color-phantom)]/20",
  flare:   "text-flare bg-[var(--color-flare-bg)] ring-[var(--color-flare)]/20",
  siren:   "text-siren bg-[var(--color-siren-bg)] ring-[var(--color-siren)]/20",
  iris:    "text-iris bg-[var(--color-iris-bg)] ring-[var(--color-iris)]/20",
  ink:     "text-ink-300 bg-ink-800/60 ring-ink-600/40",
};

const toneText: Record<Tone, string> = {
  phantom: "text-[var(--color-phantom)]",
  flare:   "text-[var(--color-flare)]",
  siren:   "text-[var(--color-siren)]",
  iris:    "text-[var(--color-iris)]",
  ink:     "text-ink-300",
};

export function Pill({
  children,
  tone = "ink",
  dot,
  className,
}: {
  children: ReactNode;
  tone?: Tone;
  dot?: boolean;
  className?: string;
}) {
  return (
    <span
      className={cn(
        "inline-flex items-center gap-1.5 h-[22px] px-2 text-[11px] rounded-full ring-1 mono",
        toneClass[tone],
        toneText[tone],
        className,
      )}
    >
      {dot && (
        <span
          className="relative w-1.5 h-1.5 rounded-full pulse-dot"
          style={{ background: "currentColor" }}
        />
      )}
      {children}
    </span>
  );
}
