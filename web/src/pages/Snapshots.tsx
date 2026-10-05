import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useMemo, useState } from "react";
import { useApi } from "../lib/auth";
import { Panel, PanelHeader } from "../components/Panel";
import { Button } from "../components/Button";
import { Empty } from "../components/Empty";
import { Pill } from "../components/Pill";
import { SearchBox } from "../components/ledger/SearchBox";
import { Pager } from "../components/ledger/Pager";
import { useListNav } from "../components/ledger/useListNav";
import { cn } from "../lib/cn";
import { humanBytes } from "../lib/format";
import type { SnapshotManifestEntry, SnapshotRecord } from "../lib/api";

const PAGE_SIZE = 50;

/**
 * Snapshot ledger: server-side search + paging + a metadata-only detail
 * pane. Detail shows id, name, size, provenance, and on-disk path —
 * content browsing is a separate concern.
 */
export function Snapshots() {
  const api = useApi();
  const [q, setQ]           = useState("");
  const [offset, setOffset] = useState(0);
  const [selected, setSel]  = useState<string | null>(null);

  useEffect(() => { setOffset(0); }, [q]);

  const { data } = useQuery({
    queryKey: ["snapshots", { q, offset }],
    queryFn:  () => api.snapshots.list({
      limit:  PAGE_SIZE,
      offset,
      q:      q.trim() || undefined,
    }),
    refetchInterval: 5000,
    placeholderData: (prev) => prev,
  });

  const rows      = data?.rows ?? [];
  const total     = data?.total ?? 0;
  const page      = Math.floor(offset / PAGE_SIZE) + 1;
  const pageCount = Math.max(1, Math.ceil(total / PAGE_SIZE));

  const { data: detail } = useQuery({
    queryKey: ["snapshot", selected],
    queryFn:  () => api.snapshots.get(selected!),
    enabled:  selected !== null,
  });

  useListNav<string>({
    rows, selected, setSelected: setSel,
    setOffset, page, pageCount, pageSize: PAGE_SIZE,
  });

  return (
    <div className="grid gap-4 grid-cols-[minmax(0,1fr)_420px] items-stretch">
      <Panel padded={false}>
        <div className="px-5 py-3 flex items-center justify-between gap-3">
          <PanelHeader eyebrow="Snapshots" title="Snapshot ledger" className="!mb-0 min-w-0" />
          <div className="flex items-center gap-2">
            <Pill tone="phantom" dot>live</Pill>
            <span className="text-[11px] mono text-ink-500">
              {total.toLocaleString()} match{total !== 1 && "es"}
            </span>
          </div>
        </div>
        <div className="hairline" />

        <div className="px-5 py-2.5 flex items-center gap-3 flex-wrap">
          <SearchBox value={q} onChange={setQ} placeholder="search id · name · workspace" />
        </div>

        <div className="hairline" />

        {rows.length === 0 ? (
          <Empty
            title={q ? "No snapshots match" : "No snapshots yet"}
            hint={q ? "Try a different query — search matches id, name, or workspace id."
                   : "Create a workspace and snapshot it to bookmark a point-in-time state."}
          />
        ) : (
          <div className="max-h-[calc(100vh-320px)] overflow-y-auto">
            {rows.map((s) => (
              <SnapshotRow
                key={s.id}
                snap={s}
                selected={s.id === selected}
                onSelect={() => setSel(s.id)}
              />
            ))}
          </div>
        )}

        <Pager
          page={page} pageCount={pageCount}
          total={total} offset={offset} size={PAGE_SIZE}
          onGo={(n) => setOffset(Math.max(0, (n - 1) * PAGE_SIZE))}
        />
      </Panel>

      <div className="h-full flex flex-col">
        <SnapshotDetail
          snap={detail ?? rows.find((r) => r.id === selected) ?? null}
          onCleared={() => setSel(null)}
        />
      </div>
    </div>
  );
}

// ─── row ─────────────────────────────────────────────────────────────────

function SnapshotRow({
  snap, selected, onSelect,
}: {
  snap: SnapshotRecord;
  selected: boolean;
  onSelect: () => void;
}) {
  const mb   = (snap.size_bytes / (1024 * 1024)).toFixed(2);
  const auto = snap.name?.startsWith("auto:");
  return (
    <button
      onClick={onSelect}
      className={cn(
        "w-full text-left px-5 py-4 grid grid-cols-[1fr_auto] gap-4 items-center hairline-after transition-colors",
        selected ? "bg-ink-800/40 ring-1 ring-inset ring-ink-700" : "hover:bg-ink-900/40",
      )}
    >
      <div className="min-w-0">
        <div className="flex items-center gap-2">
          <span className="mono text-[13px] text-ink-100 truncate">{snap.id}</span>
          {snap.name && !auto && (
            <span className="text-[11px] text-ink-400">· {snap.name}</span>
          )}
          {auto && (
            <span className="text-[10px] uppercase tracking-[0.18em] px-1.5 py-0.5 rounded bg-ink-850 text-[var(--color-iris)] ring-1 ring-[var(--color-iris)]/30">
              auto
            </span>
          )}
        </div>
        <div className="text-[11.5px] text-ink-500 mt-1 flex gap-3 flex-wrap">
          <span>created {new Date(snap.created_at).toLocaleString()}</span>
          <span>{mb} MB</span>
          {snap.workspace_id && (
            <span className="mono">from {snap.workspace_id}</span>
          )}
        </div>
      </div>
    </button>
  );
}

// ─── detail (metadata only) ─────────────────────────────────────────────

function SnapshotDetail({
  snap, onCleared,
}: {
  snap: SnapshotRecord | null;
  onCleared: () => void;
}) {
  const api = useApi();
  const qc  = useQueryClient();

  const del = useMutation({
    mutationFn: (id: string) => api.snapshots.delete(id),
    onSuccess:  () => {
      qc.invalidateQueries({ queryKey: ["snapshots"] });
      onCleared();
    },
  });

  // `from-snapshot` requires the caller to prove knowledge of the
  // parent workspace id — it's the ownership gate for rehydrate.
  // Orphaned snapshots (no recorded parent) can't be restored from the UI.
  const restore = useMutation({
    mutationFn: ({ id, parent }: { id: string; parent: string }) =>
      api.snapshots.restoreToNew(id, parent),
    onSuccess:  () => qc.invalidateQueries({ queryKey: ["workspaces"] }),
  });

  if (!snap) {
    return (
      <Panel>
        <div className="py-10 text-center text-xs text-ink-500 mono">
          select a snapshot to inspect
        </div>
      </Panel>
    );
  }

  const mb   = (snap.size_bytes / (1024 * 1024)).toFixed(2);
  const auto = snap.name?.startsWith("auto:");

  return (
    <Panel padded={false} className="flex-1 min-h-0 flex flex-col">
      <div className="px-5 py-3 flex items-start justify-between gap-3">
        <PanelHeader
          eyebrow={auto ? "Snapshot · auto" : "Snapshot"}
          title={snap.name && !auto ? snap.name : snap.id}
          className="!mb-0 min-w-0"
        />
        <div className="flex items-center gap-1.5">
          <Button
            variant="outline" size="sm"
            onClick={() => {
              if (!snap.workspace_id) return;
              restore.mutate({ id: snap.id, parent: snap.workspace_id });
            }}
            disabled={restore.isPending || !snap.workspace_id}
            title={snap.workspace_id
              ? "Rehydrate into a new workspace"
              : "Orphaned snapshot — parent workspace missing"}
          >{restore.isPending ? "restoring…" : "restore"}</Button>
          <Button
            variant="outline" size="sm"
            onClick={() => del.mutate(snap.id)}
            disabled={del.isPending}
          >{del.isPending ? "deleting…" : "delete"}</Button>
        </div>
      </div>
      <div className="hairline" />
      <div className="p-5 space-y-4 overflow-y-auto flex-1 min-h-0">
        <Section label="Identity">
          <Row label="ID"      value={snap.id} />
          {snap.name && <Row label="Name" value={snap.name} />}
          <Row label="Created" value={new Date(snap.created_at).toLocaleString()} />
          <Row label="Size"    value={`${mb} MB  ·  ${snap.size_bytes.toLocaleString()} bytes`} />
        </Section>

        {snap.workspace_id && (
          <Section label="Provenance">
            <Row label="Workspace" value={snap.workspace_id} />
            <Row label="Kind"      value={auto ? "auto (idle reaper)" : "manual"} />
          </Section>
        )}

        <Section label="Storage">
          <Row label="Path" value={snap.path} span />
        </Section>

        <ManifestSection id={snap.id} />
      </div>
    </Panel>
  );
}

// ─── manifest (pool-backed snapshots) ───────────────────────────────────

function ManifestSection({ id }: { id: string }) {
  const api = useApi();
  const [filter, setFilter] = useState("");
  // Reset path filter when the user selects a different snapshot so
  // stale filter state doesn't hide everything in the new manifest.
  useEffect(() => { setFilter(""); }, [id]);
  const { data, isLoading, error } = useQuery({
    queryKey: ["snapshot-manifest", id],
    queryFn:  () => api.snapshots.manifest(id),
  });

  if (isLoading) {
    return (
      <div>
        <div className="text-[10px] font-medium uppercase tracking-[0.2em] text-ink-400 mb-2">
          Files
        </div>
        <div className="text-[11.5px] text-ink-500 mono">loading…</div>
      </div>
    );
  }
  if (error || !data) {
    return (
      <div>
        <div className="text-[10px] font-medium uppercase tracking-[0.2em] text-ink-400 mb-2">
          Files
        </div>
        <div className="text-[11.5px] text-[var(--color-siren)] mono">
          manifest failed · {error instanceof Error ? error.message : "no data"}
        </div>
      </div>
    );
  }

  if (data.kind === "classic") {
    return (
      <div>
        <div className="text-[10px] font-medium uppercase tracking-[0.2em] text-ink-400 mb-2">
          Files
        </div>
        <div className="text-[11.5px] text-ink-500">
          full-copy snapshot — file listing isn't persisted. Set{" "}
          <span className="mono text-ink-300">AGENTJAIL_SNAPSHOT_POOL_DIR</span>{" "}
          to enable content-addressed snapshots with manifests.
        </div>
      </div>
    );
  }

  return <ManifestList entries={data.entries} filter={filter} setFilter={setFilter} />;
}

function ManifestList({
  entries, filter, setFilter,
}: {
  entries: SnapshotManifestEntry[];
  filter: string;
  setFilter: (v: string) => void;
}) {
  const filtered = useMemo(() => {
    const f = filter.trim().toLowerCase();
    if (!f) return entries;
    return entries.filter((e) => e.path.toLowerCase().includes(f));
  }, [entries, filter]);
  const totalBytes = useMemo(() => entries.reduce((s, e) => s + e.size, 0), [entries]);

  return (
    <div>
      <div className="flex items-center justify-between mb-2">
        <div className="text-[10px] font-medium uppercase tracking-[0.2em] text-ink-400">
          Files
        </div>
        <div className="text-[10px] mono text-ink-500">
          {entries.length.toLocaleString()} files · {humanBytes(totalBytes)}
        </div>
      </div>
      <div className="mb-2">
        <input
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
          placeholder="filter path…"
          className="w-full h-7 px-2.5 rounded bg-ink-900/70 ring-1 ring-ink-800 focus:ring-ink-600 outline-none text-[11.5px] text-ink-200 placeholder:text-ink-600 mono"
        />
      </div>
      {filtered.length === 0 ? (
        <div className="text-[11.5px] text-ink-500 mono">no matches</div>
      ) : (
        <div className="max-h-[50vh] overflow-y-auto rounded border border-ink-800">
          <table className="w-full text-[11px] mono">
            <thead className="sticky top-0 bg-ink-900/95 backdrop-blur">
              <tr className="text-ink-500 text-left">
                <th className="px-3 py-1.5 font-normal">path</th>
                <th className="px-3 py-1.5 font-normal w-[90px] text-right">size</th>
                <th className="px-3 py-1.5 font-normal w-[60px]">mode</th>
              </tr>
            </thead>
            <tbody>
              {filtered.slice(0, 2000).map((e) => (
                <tr key={e.hash + e.path} className="hover:bg-ink-900/60">
                  <td className="px-3 py-1 text-ink-200 truncate max-w-0">{e.path}</td>
                  <td className="px-3 py-1 text-right text-ink-400">{humanBytes(e.size)}</td>
                  <td className="px-3 py-1 text-ink-500">{(e.mode & 0o777).toString(8).padStart(3, "0")}</td>
                </tr>
              ))}
            </tbody>
          </table>
          {filtered.length > 2000 && (
            <div className="px-3 py-1.5 text-[10.5px] text-ink-600 bg-ink-900/60">
              showing first 2000 of {filtered.length.toLocaleString()} — narrow the filter
            </div>
          )}
        </div>
      )}
    </div>
  );
}

function Section({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div>
      <div className="text-[10px] font-medium uppercase tracking-[0.2em] text-ink-400 mb-2">
        {label}
      </div>
      <div className="grid gap-1.5">{children}</div>
    </div>
  );
}

function Row({ label, value, span }: { label: string; value: string; span?: boolean }) {
  // `span` = value is long (paths, URLs). Stack label above the value so
  // the value gets the full panel width + wraps cleanly.
  if (span) {
    return (
      <div className="text-[12px]">
        <div className="text-ink-500 mb-0.5">{label}</div>
        <div className="mono text-ink-100 break-all">{value}</div>
      </div>
    );
  }
  return (
    <div className="grid grid-cols-[80px_1fr] gap-3 items-baseline text-[12px]">
      <span className="text-ink-500">{label}</span>
      <span className="mono text-ink-100 break-all">{value}</span>
    </div>
  );
}
