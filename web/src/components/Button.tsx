import type { ButtonHTMLAttributes, ReactNode } from "react";
import { cn } from "../lib/cn";

type Variant = "primary" | "ghost" | "danger" | "outline";

const variants: Record<Variant, string> = {
  primary:
    "bg-[var(--color-phantom)] text-ink-950 hover:brightness-110 shadow-[0_6px_24px_-8px_var(--color-phantom)]",
  ghost:
    "text-ink-200 hover:text-ink-100 hover:bg-ink-800/60",
  danger:
    "text-[var(--color-siren)] hover:bg-[var(--color-siren-bg)]",
  outline:
    "ring-1 ring-ink-600 text-ink-200 hover:ring-ink-500 hover:text-ink-100 bg-ink-850/60",
};

interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: Variant;
  icon?: ReactNode;
  size?: "sm" | "md";
}

export function Button({
  variant = "outline",
  icon,
  size = "md",
  className,
  children,
  ...rest
}: ButtonProps) {
  return (
    <button
      className={cn(
        "inline-flex items-center gap-2 rounded-lg font-medium transition-all",
        "disabled:opacity-50 disabled:cursor-not-allowed",
        size === "sm" ? "h-7 px-2.5 text-xs" : "h-9 px-3.5 text-[13px]",
        variants[variant],
        className,
      )}
      {...rest}
    >
      {icon}
      {children}
    </button>
  );
}
