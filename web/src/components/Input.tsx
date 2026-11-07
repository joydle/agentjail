import type { InputHTMLAttributes, ReactNode } from "react";
import { cn } from "../lib/cn";

interface FieldProps extends Omit<InputHTMLAttributes<HTMLInputElement>, "prefix"> {
  label?: string;
  prefix?: ReactNode;
  hint?: string;
  error?: string;
}

export function Field({ label, prefix, hint, error, className, ...rest }: FieldProps) {
  return (
    <label className={cn("block", className)}>
      {label && (
        <div className="flex items-center gap-2 mb-1.5">
          <span className="text-[10px] uppercase tracking-[0.22em] text-ink-400 font-medium">
            {label}
          </span>
          {hint && <span className="text-[10px] text-ink-500">{hint}</span>}
        </div>
      )}
      <div
        className={cn(
          "flex items-center gap-2 h-10 px-3 rounded-lg",
          "bg-ink-900/70 ring-1 ring-ink-700 focus-within:ring-ink-500 transition-shadow",
          error && "ring-[var(--color-siren)]/50 focus-within:ring-[var(--color-siren)]",
        )}
      >
        {prefix && <span className="text-ink-400 mono text-xs">{prefix}</span>}
        <input
          className="flex-1 min-w-0 bg-transparent outline-none placeholder:text-ink-500 text-ink-100 text-sm"
          {...rest}
        />
      </div>
      {error && (
        <div className="mt-1 text-[11px] text-[var(--color-siren)]">{error}</div>
      )}
    </label>
  );
}
