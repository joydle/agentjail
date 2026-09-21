import { useQuery } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import { useApi } from "../lib/auth";
import type { JailKind, JailStatus } from "../lib/api";
import { Panel, PanelHeader } from "../components/Panel";
import { Pill } from "../components/Pill";
import { JailsList } from "../components/jails/JailsList";
import { JailDetail } from "../components/jails/JailDetail";
import { cn } from "../lib/cn";

const STATUS_FILTERS: { id: JailStatus | "all"; label: string }[] = [
  { id: "all",       label: "all"       },
  { id: "running",   label: "running"   },
  { id: "completed", label: "ok"        },
  { id: "error",     label: "error"     },
];

const KIND_FILTERS: { id: JailKind | "all"; label: string }[] = [
  { id: "all",    label: "all kinds" },
  { id: "run",    label: "run"       },
  { id: "exec",   label: "exec"      },
  { id: "fork",   label: "fork"      },
  { id: "stream", label: "stream"    },
];

const PAGE_SIZE = 50;

/** Jail-run ledger with live tail, search, kind/status filters, server-side paging. */
export function Jails() {
  const api = useApi();
  const [status,   setStatus]   = useState<JailStatus | "all">("all");
  const [kind,     setKind]     = useState<JailKind   | "all">("all");
  const [q,        setQ]        = useState("");
  const [offset,   setOffset]   = useState(0);
  const [selected, setSelected] = useState<number | null>(null);

  // Reset pagination when any filter changes.
  useEffect(() => { setOffset(0); }, [status, kind, q]);

  const { data } = useQuery({
    queryKey: ["jails", { status, kind, q, offset }],
    queryFn:  () => api.jails.list({
      limit:  PAGE_SIZE,
      offset,
      status: status === "all" ? undefined : status,
      kind:   kind   === "all" ? undefined : kind,
      q:      q.trim() || undefined,
    }),
    refetchInterval: 2000,
    placeholderData: (prev) => prev,
  });

  const rows = data?.rows ?? [];
  const total = data?.total ?? 0;
  const page = Math.floor(offset / PAGE_SIZE) + 1;
  const pageCount = Math.max(1, Math.ceil(total / PAGE_SIZE));

  // Live detail poll — 1s while running, else on selection change only.
  const { data: detail } = useQuery({
    queryKey: ["jail", selected],
    queryFn:  () => api.jails.get(selected!),
    enabled:  selected !== null,
    refetchInterval: (q) => q.state.data?.status === "running" ? 1000 : false,
  });

  // Keyboard nav: j/k move selection, [ ] page, / focus search.
  useKeyboardNav({ rows, selected, setSelected, setOffset, pageCount, page });

  return (
    <div className="grid gap-4 grid-cols-[minmax(0,1fr)_600px] items-stretch">
      <Panel padded={false}>
        <div className="px-5 py-3 flex items-center justify-between gap-3">
          <PanelHeader
            eyebrow="Jails"
            title="Jail run ledger"
            className="!mb-0 min-w-0"
          />
          <div className="flex items-center gap-2">
            <Pill tone="phantom" dot>live</Pill>
            <span className="text-[11px] mono text-ink-500">
              {total.toLocaleString()} match{total !== 1 && "es"}
            </span>
          </div>
        </div>
        <div className="hairline" />

        <Toolbar
          q={q} setQ={setQ}
          status={status} setStatus={setStatus}
          kind={kind}     setKind={setKind}
        />

        <div className="hairline" />
        <div className="max-h-[calc(100vh-300px)] overflow-y-auto">
          <JailsList
            rows={rows}
            selected={selected ?? undefined}
            onSelect={setSelected}
          />
        </div>

        <Pager
          page={page} pageCount={pageCount}
          total={total} offset={offset} size={PAGE_SIZE}
          onGo={(n) => setOffset(Math.max(0, (n - 1) * PAGE_SIZE))}
        />
      </Panel>

      <div className="space-y-4">
        <JailDetail rec={detail ?? null} onSelect={setSelected} />
      </div>
    </div>
  );
}

// ─── toolbar ─────────────────────────────────────────────────────────────

function Toolbar({
  q, setQ,
  status, setStatus,
  kind,   setKind,
}: {
  q: string; setQ: (s: string) => void;
  status: JailStatus | "all"; setStatus: (s: JailStatus | "all") => void;
  kind:   JailKind   | "all"; setKind:   (k: JailKind   | "all") => void;
}) {
  return (
    <div className="px-5 py-2.5 flex items-center gap-3 flex-wrap">
      <SearchBox value={q} onChange={setQ} />
      <div className="flex items-center gap-1 ml-auto">
        {STATUS_FILTERS.map((f) => (
          <Chip key={f.id} on={status === f.id} onClick={() => setStatus(f.id)}>
            {f.label}
          </Chip>
        ))}
      </div>
      <div className="flex items-center gap-1">
        {KIND_FILTERS.map((f) => (
          <Chip key={f.id} on={kind === f.id} onClick={() => setKind(f.id)} tone="iris">
            {f.label}
          </Chip>
        ))}
      </div>
    </div>
  );
}

function SearchBox({ value, onChange }: { value: string; onChange: (v: string) => void }) {
  return (
    <div className="flex items-center gap-2 h-8 px-2.5 rounded-md bg-ink-900/70 ring-1 ring-ink-800 focus-within:ring-ink-600 transition min-w-[280px]">
      <span className="text-ink-500 text-[11px]">⌕</span>
      <input
        data-search-input
        value={value}
        onChange={(e) => onChange(e.target.value)}
        placeholder="search label · session · error"
        className="flex-1 bg-transparent outline-none text-[12px] text-ink-200 placeholder:text-ink-600"
      />
      {value && (
        <button
          onClick={() => onChange("")}
          className="text-ink-500 hover:text-ink-200 text-[11px] leading-none"
          aria-label="clear search"
        >×</button>
      )}
      <span className="text-[9.5px] mono text-ink-600 tracking-wider">/ FOCUS</span>
    </div>
  );
}

function Chip({
  on, onClick, children, tone = "phantom",
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

// ─── pager ───────────────────────────────────────────────────────────────

function Pager({
  page, pageCount, total, offset, size, onGo,
}: {
  page: number; pageCount: number;
  total: number; offset: number; size: number;
  onGo: (p: number) => void;
}) {
  const start = total === 0 ? 0 : offset + 1;
  const end   = Math.min(offset + size, total);
  return (
    <div className="px-5 py-2 flex items-center justify-between border-t border-ink-800 text-[11px] mono text-ink-500">
      <span>{start.toLocaleString()}–{end.toLocaleString()} of {total.toLocaleString()}</span>
      <div className="flex items-center gap-1">
        <PagerBtn onClick={() => onGo(1)}         disabled={page <= 1}>« first</PagerBtn>
        <PagerBtn onClick={() => onGo(page - 1)}  disabled={page <= 1}>[</PagerBtn>
        <span className="px-3 text-ink-300">{page} / {pageCount}</span>
        <PagerBtn onClick={() => onGo(page + 1)}  disabled={page >= pageCount}>]</PagerBtn>
        <PagerBtn onClick={() => onGo(pageCount)} disabled={page >= pageCount}>last »</PagerBtn>
      </div>
    </div>
  );
}

function PagerBtn({
  onClick, disabled, children,
}: { onClick: () => void; disabled: boolean; children: React.ReactNode }) {
  return (
    <button
      onClick={onClick}
      disabled={disabled}
      className="h-6 px-2 rounded text-[11px] mono text-ink-400 hover:text-ink-100 disabled:opacity-30 disabled:cursor-not-allowed"
    >{children}</button>
  );
}

// ─── keyboard nav ────────────────────────────────────────────────────────

function useKeyboardNav({
  rows, selected, setSelected, setOffset, pageCount, page,
}: {
  rows: { id: number }[];
  selected: number | null;
  setSelected: (id: number) => void;
  setOffset: (fn: (o: number) => number) => void;
  pageCount: number;
  page: number;
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
        if (page < pageCount) setOffset((o) => o + PAGE_SIZE);
      } else if (e.key === "[") {
        if (page > 1) setOffset((o) => Math.max(0, o - PAGE_SIZE));
      }
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [rows, selected, setSelected, setOffset, page, pageCount]);
}
