import type { ReactNode } from "react";
import { cn } from "../lib/cn";

interface PanelProps {
  children: ReactNode;
  className?: string;
  frosted?: boolean;
  padded?: boolean;
}

export function Panel({ children, className, frosted, padded = true }: PanelProps) {
  return (
    <div className={cn("panel", frosted && "panel-frosted", padded && "p-5", className)}>
      {children}
    </div>
  );
}

interface PanelHeaderProps {
  title: string;
  eyebrow?: string;
  action?: ReactNode;
  className?: string;
}

export function PanelHeader({ title, eyebrow, action, className }: PanelHeaderProps) {
  return (
    <div className={cn("flex items-start justify-between gap-3 mb-4", className)}>
      <div>
        {eyebrow && (
          <div className="text-[10px] font-medium uppercase tracking-[0.2em] text-ink-400 mb-1">
            {eyebrow}
          </div>
        )}
        <h2 className="text-base font-semibold text-ink-100 display">{title}</h2>
      </div>
      {action && <div>{action}</div>}
    </div>
  );
}
