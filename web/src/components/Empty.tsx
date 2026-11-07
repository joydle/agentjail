import type { ReactNode } from "react";

export function Empty({
  title,
  hint,
  action,
}: {
  title: string;
  hint?: string;
  action?: ReactNode;
}) {
  return (
    <div className="py-10 px-6 text-center">
      <div className="mx-auto mb-4 w-10 h-10 rounded-full grid place-items-center ring-1 ring-ink-700 bg-ink-850">
        <span className="w-1.5 h-1.5 rounded-full bg-ink-500" />
      </div>
      <div className="text-sm text-ink-200 font-medium">{title}</div>
      {hint && <div className="mt-1 text-xs text-ink-400 max-w-sm mx-auto">{hint}</div>}
      {action && <div className="mt-4 inline-flex">{action}</div>}
    </div>
  );
}
