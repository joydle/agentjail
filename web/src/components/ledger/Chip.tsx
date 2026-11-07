import { cn } from "../../lib/cn";

/** Ledger toolbar filter chip. Toggles between active / dim states. */
export function Chip({
  on,
  onClick,
  children,
  tone = "phantom",
}: {
  on: boolean;
  onClick: () => void;
  children: React.ReactNode;
  tone?: "phantom" | "iris";
}) {
  return (
    <button
      onClick={onClick}
      className={cn(
        "h-7 px-2.5 rounded-full text-[11px] mono transition-colors whitespace-nowrap",
        on
          ? (tone === "iris"
              ? "bg-[var(--color-iris)] text-ink-950"
              : "bg-ink-100 text-ink-950")
          : "text-ink-400 hover:text-ink-200 ring-1 ring-ink-800",
      )}
    >
      {children}
    </button>
  );
}
