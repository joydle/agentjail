import { useEffect } from "react";

/**
 * Shared keyboard nav for ledger-style pages.
 * `/` focuses the search box (any element with `data-search-input`);
 * `j` / `ArrowDown` and `k` / `ArrowUp` move selection by id;
 * `]` / `[` page forward / back.
 */
export function useListNav<TId>({
  rows,
  selected,
  setSelected,
  setOffset,
  page,
  pageCount,
  pageSize,
}: {
  rows: { id: TId }[];
  selected: TId | null;
  setSelected: (id: TId) => void;
  setOffset: (fn: (o: number) => number) => void;
  page: number;
  pageCount: number;
  pageSize: number;
}) {
  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      const t = e.target as HTMLElement | null;
      if (t && (t.tagName === "INPUT" || t.tagName === "TEXTAREA")) {
        if (e.key === "Escape") (t as HTMLInputElement).blur();
        return;
      }
      if (e.key === "/") {
        e.preventDefault();
        (document.querySelector("[data-search-input]") as HTMLInputElement | null)?.focus();
      } else if (e.key === "j" || e.key === "ArrowDown") {
        e.preventDefault();
        const i = rows.findIndex((r) => r.id === selected);
        const next = rows[Math.min(rows.length - 1, i < 0 ? 0 : i + 1)];
        if (next) setSelected(next.id);
      } else if (e.key === "k" || e.key === "ArrowUp") {
        e.preventDefault();
        const i = rows.findIndex((r) => r.id === selected);
        const prev = rows[Math.max(0, i <= 0 ? 0 : i - 1)];
        if (prev) setSelected(prev.id);
      } else if (e.key === "]") {
        if (page < pageCount) setOffset((o) => o + pageSize);
      } else if (e.key === "[") {
        if (page > 1) setOffset((o) => Math.max(0, o - pageSize));
      }
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [rows, selected, setSelected, setOffset, page, pageCount, pageSize]);
}
