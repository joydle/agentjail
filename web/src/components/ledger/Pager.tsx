/**
 * Ledger pager — first/prev/next/last + live "N–M of T" counter.
 * Keyboard shortcuts `[` / `]` for prev/next are caller-wired (see
 * `useListNav`).
 */
export function Pager({
  page,
  pageCount,
  total,
  offset,
  size,
  onGo,
}: {
  page: number;
  pageCount: number;
  total: number;
  offset: number;
  size: number;
  onGo: (p: number) => void;
}) {
  const start = total === 0 ? 0 : offset + 1;
  const end   = Math.min(offset + size, total);
  return (
    <div className="px-5 py-2 flex items-center justify-between border-t border-ink-800 text-[11px] mono text-ink-500">
      <span>{start.toLocaleString()}–{end.toLocaleString()} of {total.toLocaleString()}</span>
      <div className="flex items-center gap-1">
        <Btn onClick={() => onGo(1)}         disabled={page <= 1}        aria-label="first page">« first</Btn>
        <Btn onClick={() => onGo(page - 1)}  disabled={page <= 1}        aria-label="previous page (shortcut [)">[</Btn>
        <span className="px-3 text-ink-300">{page} / {pageCount}</span>
        <Btn onClick={() => onGo(page + 1)}  disabled={page >= pageCount} aria-label="next page (shortcut ])">]</Btn>
        <Btn onClick={() => onGo(pageCount)} disabled={page >= pageCount} aria-label="last page">last »</Btn>
      </div>
    </div>
  );
}

function Btn({
  onClick,
  disabled,
  children,
  "aria-label": ariaLabel,
}: {
  onClick: () => void;
  disabled: boolean;
  children: React.ReactNode;
  "aria-label"?: string;
}) {
  return (
    <button
      onClick={onClick}
      disabled={disabled}
      aria-label={ariaLabel}
      className="h-6 px-2 rounded text-[11px] mono text-ink-400 hover:text-ink-100 disabled:opacity-30 disabled:cursor-not-allowed"
    >{children}</button>
  );
}
