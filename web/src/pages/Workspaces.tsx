import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import { useApi } from "../lib/auth";
import { Link } from "react-router-dom";
import { Panel, PanelHeader } from "../components/Panel";
import { Button } from "../components/Button";
import { Empty } from "../components/Empty";
import { Field } from "../components/Input";
import { Pill } from "../components/Pill";
import { SearchBox } from "../components/ledger/SearchBox";
import { Pager } from "../components/ledger/Pager";
import { useListNav } from "../components/ledger/useListNav";
import { humanMs, timeAgo } from "../lib/format";
import { cn } from "../lib/cn";
import type { JailRecord, Workspace, WorkspaceCreateRequest } from "../lib/api";

const PAGE_SIZE = 50;

/**
 * Truncate a `wrk_<24hex>` id to `wrk_abcd…wxyz` for inline display
 * alongside the label. Keeps the last 4 chars so two workspaces that
 * share a label remain visually distinguishable.
 */
function shortWsId(id: string): string {
  if (id.length <= 12) return id;
  const head = id.slice(0, 8);
  const tail = id.slice(-4);
  return `${head}…${tail}`;
}

export function Workspaces() {
  const api = useApi();
  const [q, setQ]             = useState("");
  const [offset, setOffset]   = useState(0);
  const [selected, setSel]    = useState<string | null>(null);
  const [mode, setMode]       = useState<"create" | "detail">("create");

  // Reset pagination when the filter changes.
  useEffect(() => { setOffset(0); }, [q]);

  const { data } = useQuery({
    queryKey: ["workspaces", { q, offset }],
    queryFn:  () => api.workspaces.list({
      limit:  PAGE_SIZE,
      offset,
      q:      q.trim() || undefined,
    }),
    refetchInterval: 4000,
    placeholderData: (prev) => prev,
  });

  const rows      = data?.rows ?? [];
  const total     = data?.total ?? 0;
  const page      = Math.floor(offset / PAGE_SIZE) + 1;
  const pageCount = Math.max(1, Math.ceil(total / PAGE_SIZE));

  const { data: detail } = useQuery({
    queryKey: ["workspace", selected],
    queryFn:  () => api.workspaces.get(selected!),
    enabled:  selected !== null,
  });

  useListNav<string>({
    rows, selected, setSelected: setSel,
    setOffset, page, pageCount, pageSize: PAGE_SIZE,
  });

  // Zero-state: no filter applied + no projects at all → welcome wizard.
  const isZero = !q && total === 0 && data !== undefined;
  if (isZero) {
    return <ProjectsWelcome />;
  }

  return (
    <div className="grid gap-4 grid-cols-[minmax(0,1fr)_420px] items-stretch">
      <Panel padded={false}>
        <div className="px-5 py-3 flex items-center justify-between gap-3">
          <PanelHeader eyebrow="Projects" title="Your projects" className="!mb-0 min-w-0" />
          <div className="flex items-center gap-2">
            <Pill tone="phantom" dot>live</Pill>
            <span className="text-[11px] mono text-ink-500">
              {total.toLocaleString()} match{total !== 1 && "es"}
            </span>
          </div>
        </div>
        <div className="hairline" />

        <div className="px-5 py-2.5 flex items-center gap-3 flex-wrap">
          <SearchBox value={q} onChange={setQ} placeholder="search id · label · repo" />
          <div className="ml-auto flex items-center gap-2">
            <ModeToggle mode={mode} setMode={setMode} />
          </div>
        </div>

        <div className="hairline" />

        {rows.length === 0 ? (
          <Empty
            title={q ? "No projects match" : "No projects yet"}
            hint={q ? "Try a different query — search matches id, label, or git repo."
                   : "Create one on the right — clone a repo, run multi-step builds, snapshot the result."}
          />
        ) : (
          <div className="max-h-[calc(100vh-320px)] overflow-y-auto">
            {rows.map((w) => (
              <WorkspaceRow
                key={w.id}
                ws={w}
                selected={w.id === selected}
                onSelect={() => {
                  setSel(w.id);
                  setMode("detail");
                }}
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
        {mode === "detail" && selected ? (
          <WorkspaceDetail
            ws={detail ?? rows.find((r) => r.id === selected) ?? null}
            onBackToCreate={() => { setSel(null); setMode("create"); }}
          />
        ) : (
          <CreateWorkspaceForm />
        )}
      </div>
    </div>
  );
}

// ─── toolbar: detail vs create toggle ───────────────────────────────────

function ModeToggle({
  mode, setMode,
}: {
  mode: "create" | "detail";
  setMode: (m: "create" | "detail") => void;
}) {
  return (
    <div className="flex items-center gap-1 rounded-full ring-1 ring-ink-800 p-0.5">
      <ToggleBtn on={mode === "create"} onClick={() => setMode("create")}>create</ToggleBtn>
      <ToggleBtn on={mode === "detail"} onClick={() => setMode("detail")}>detail</ToggleBtn>
    </div>
  );
}

function ToggleBtn({
  on, onClick, children,
}: { on: boolean; onClick: () => void; children: React.ReactNode }) {
  return (
    <button
      onClick={onClick}
      className={cn(
        "h-6 px-3 rounded-full text-[11px] mono transition-colors",
        on ? "bg-ink-100 text-ink-950" : "text-ink-400 hover:text-ink-200",
      )}
    >{children}</button>
  );
}

// ─── row ─────────────────────────────────────────────────────────────────

/** Label convention: workspaces auto-restored from a snapshot carry
 *  `restored from snap_<hex>`. Parse it once so the row can show a
 *  distinct chip + short snapshot id instead of a 60-char string. */
function parseRestoredFrom(label: string | null): string | null {
  if (!label) return null;
  const m = label.match(/^restored from (snap_[a-f0-9]+)$/);
  return m ? m[1] : null;
}

function shortSnapId(id: string): string {
  if (id.length <= 18) return id;
  return `${id.slice(0, 9)}…${id.slice(-4)}`;
}

function RestoreGlyph() {
  // 14×14 curved arrow — signals "came back from" at a glance.
  return (
    <svg width="14" height="14" viewBox="0 0 14 14" aria-hidden
      className="shrink-0 text-[var(--color-iris)]">
      <path d="M3 7 A4 4 0 1 1 7 11" fill="none" stroke="currentColor"
        strokeWidth="1.4" strokeLinecap="round" />
      <path d="M3 7 L1.5 5.4 M3 7 L4.6 5.4" fill="none" stroke="currentColor"
        strokeWidth="1.4" strokeLinecap="round" />
    </svg>
  );
}

function WorkspaceRow({
  ws, selected, onSelect,
}: {
  ws: Workspace;
  selected: boolean;
  onSelect: () => void;
}) {
  const paused = ws.paused_at !== null;
  const restoredFrom = parseRestoredFrom(ws.label);
  return (
    <button
      onClick={onSelect}
      className={cn(
        "w-full text-left px-5 py-4 grid grid-cols-[1fr_auto] gap-4 items-center hairline-after transition-colors",
        selected ? "bg-ink-800/40 ring-1 ring-inset ring-ink-700" : "hover:bg-ink-900/40",
      )}
    >
      <div className="min-w-0">
        <div className="flex items-center gap-2 min-w-0">
          {restoredFrom ? (
            <>
              <span
                className="inline-flex items-center gap-1.5 shrink-0 h-[22px] px-2 rounded-full
                           ring-1 ring-[var(--color-iris)]/30
                           bg-[var(--color-iris-bg)] text-[var(--color-iris)]
                           text-[11px] mono"
                title={`restored from ${restoredFrom}`}
              >
                <RestoreGlyph />
                restored
              </span>
              <span className="mono text-[11.5px] text-ink-300 truncate" title={restoredFrom}>
                {shortSnapId(restoredFrom)}
              </span>
              <span className="mono text-[10px] text-ink-500 shrink-0">{shortWsId(ws.id)}</span>
            </>
          ) : ws.label ? (
            <>
              <span className="text-[13px] text-ink-100 truncate">{ws.label}</span>
              <span className="mono text-[10px] text-ink-500 shrink-0">{shortWsId(ws.id)}</span>
            </>
          ) : (
            <span className="mono text-[13px] text-ink-100 truncate">{ws.id}</span>
          )}
          {paused && (
            <span className="text-[10px] uppercase tracking-[0.18em] px-1.5 py-0.5 rounded bg-ink-850 text-[var(--color-flare)] ring-1 ring-[var(--color-flare)]/30">
              paused
            </span>
          )}
          {ws.config.idle_timeout_secs > 0 && !paused && (
            <span className="text-[10px] uppercase tracking-[0.18em] px-1.5 py-0.5 rounded bg-ink-850 text-ink-400 ring-1 ring-ink-700">
              idle {ws.config.idle_timeout_secs}s
            </span>
          )}
        </div>
        <div className="text-[11.5px] text-ink-500 mt-1 flex gap-3 flex-wrap">
          <span>created {new Date(ws.created_at).toLocaleString()}</span>
          {ws.last_exec_at && (
            <span>last exec {new Date(ws.last_exec_at).toLocaleString()}</span>
          )}
          {ws.git_repo && (
            <span className="mono truncate">{ws.git_repo}</span>
          )}
        </div>
      </div>
    </button>
  );
}

// ─── detail (metadata only) ─────────────────────────────────────────────

function WorkspaceDetail({
  ws,
  onBackToCreate,
}: {
  ws: Workspace | null;
  onBackToCreate: () => void;
}) {
  const api = useApi();
  const qc  = useQueryClient();
  const [showAdvanced, setShowAdvanced] = useState(false);
  const [renaming, setRenaming]         = useState(false);
  const [labelDraft, setLabelDraft]     = useState("");

  const del = useMutation({
    mutationFn: (id: string) => api.workspaces.delete(id),
    onSuccess:  () => {
      qc.invalidateQueries({ queryKey: ["workspaces"] });
      onBackToCreate();
    },
  });

  const rename = useMutation({
    mutationFn: ({ id, label }: { id: string; label: string }) =>
      api.workspaces.rename(id, label),
    onSuccess: (fresh) => {
      qc.setQueryData(["workspace", fresh.id], fresh);
      qc.invalidateQueries({ queryKey: ["workspaces"] });
      setRenaming(false);
    },
  });

  if (!ws) {
    return (
      <Panel>
        <div className="py-10 text-center text-xs text-ink-500 mono">
          select a project to inspect
        </div>
      </Panel>
    );
  }

  const paused = ws.paused_at !== null;
  return (
    <Panel padded={false} className="flex-1 min-h-0 flex flex-col">
      <div className="px-5 py-3 flex items-start justify-between gap-3">
        <div className="min-w-0 flex-1">
          <div className="text-[10px] uppercase tracking-[0.22em] text-ink-400 mb-1">Project</div>
          {renaming ? (
            <form
              className="flex items-center gap-2"
              onSubmit={(e) => {
                e.preventDefault();
                rename.mutate({ id: ws.id, label: labelDraft });
              }}
            >
              <input
                autoFocus
                value={labelDraft}
                onChange={(e) => setLabelDraft(e.target.value)}
                onKeyDown={(e) => { if (e.key === "Escape") setRenaming(false); }}
                placeholder="label"
                className="bg-ink-900 ring-1 ring-ink-700 rounded px-2 py-1 text-[13px] text-ink-100 focus:outline-none focus:ring-phantom min-w-0 flex-1"
              />
              <button type="submit" className="text-[11px] mono text-phantom hover:text-ink-100" disabled={rename.isPending}>
                {rename.isPending ? "…" : "save"}
              </button>
              <button type="button" className="text-[11px] mono text-ink-500 hover:text-ink-200" onClick={() => setRenaming(false)}>
                cancel
              </button>
            </form>
          ) : (
            <div className="flex items-center gap-2 min-w-0">
              <h3 className="display text-[18px] leading-tight font-semibold truncate">
                {ws.label ?? ws.id}
              </h3>
              <button
                type="button"
                onClick={() => { setLabelDraft(ws.label ?? ""); setRenaming(true); }}
                className="text-[11px] mono text-ink-500 hover:text-ink-200 shrink-0"
                title="Rename project"
              >
                rename
              </button>
            </div>
          )}
          {ws.label && (
            <div className="mono text-[11px] text-ink-500 mt-0.5 truncate">{ws.id}</div>
          )}
          {rename.error && (
            <div className="text-[11px] text-[var(--color-siren)] mt-1">
              {rename.error instanceof Error ? rename.error.message : String(rename.error)}
            </div>
          )}
        </div>
        <Button variant="outline" size="sm" onClick={() => del.mutate(ws.id)} disabled={del.isPending}>
          {del.isPending ? "deleting…" : "delete"}
        </Button>
      </div>
      <div className="hairline" />
      <div className="p-5 space-y-4 overflow-y-auto flex-1 min-h-0">
        <Section label="Identity">
          <Row label="ID"         value={ws.id} />
          {ws.label && <Row label="Label" value={ws.label} />}
          <Row label="Created"    value={new Date(ws.created_at).toLocaleString()} />
          {ws.last_exec_at && (
            <Row label="Last exec" value={new Date(ws.last_exec_at).toLocaleString()} />
          )}
          <Row
            label="Status"
            value={paused ? `paused @ ${new Date(ws.paused_at!).toLocaleString()}` : "active"}
            tone={paused ? "flare" : "phantom"}
          />
          {ws.auto_snapshot && (
            <Row label="Auto-snap" value={ws.auto_snapshot} />
          )}
        </Section>

        {ws.git_repo && (
          <Section label="Git seed">
            <Row label="Repo" value={ws.git_repo} span />
            {ws.git_ref && <Row label="Ref" value={ws.git_ref} />}
          </Section>
        )}

        {ws.domains.length > 0 && (
          <Section label="Gateway routes">
            {ws.domains.map((d, i) => (
              <Row
                key={i}
                label={d.domain}
                value={
                  d.backend_url
                    ? d.backend_url
                    : d.vm_port != null
                      ? `→ jail :${d.vm_port}`
                      : "—"
                }
                span
              />
            ))}
          </Section>
        )}

        <RecentExecs wsId={ws.id} />

        <div>
          <button
            type="button"
            onClick={() => setShowAdvanced((x) => !x)}
            aria-expanded={showAdvanced}
            className="text-[11px] mono text-ink-400 hover:text-ink-200 flex items-center gap-1.5"
          >
            <span className={cn("inline-block transition-transform", showAdvanced && "rotate-90")}>›</span>
            {showAdvanced ? "hide" : "show"} technical config
          </button>
        </div>

        {showAdvanced && (
          <>
            <Section label="Config">
              <Row label="Memory"  value={`${ws.config.memory_mb} MB`} />
              <Row label="Timeout" value={`${ws.config.timeout_secs} s`} />
              <Row label="CPU"     value={`${ws.config.cpu_percent}%`} />
              <Row label="Max PIDs" value={String(ws.config.max_pids)} />
              <Row label="Seccomp"  value={ws.config.seccomp} />
              <Row
                label="Network"
                value={
                  ws.config.network_mode === "allowlist"
                    ? `allowlist · ${ws.config.network_domains.length}`
                    : ws.config.network_mode
                }
              />
              <Row
                label="Idle"
                value={ws.config.idle_timeout_secs === 0 ? "never" : `${ws.config.idle_timeout_secs} s`}
              />
            </Section>

            {ws.config.network_mode === "allowlist" && ws.config.network_domains.length > 0 && (
              <Section label="Allowlist">
                <div className="flex flex-wrap gap-1.5">
                  {ws.config.network_domains.map((d) => (
                    <span key={d} className="mono text-[11px] text-phantom bg-ink-800/60 rounded px-2 py-0.5">{d}</span>
                  ))}
                </div>
              </Section>
            )}

            {ws.config.flavors && ws.config.flavors.length > 0 && (
              <Section label="Flavors">
                <div className="flex flex-wrap gap-1.5">
                  {ws.config.flavors.map((f) => (
                    <span key={f} className="mono text-[11px] text-phantom bg-ink-800/60 rounded px-2 py-0.5">{f}</span>
                  ))}
                </div>
              </Section>
            )}

          </>
        )}
      </div>
    </Panel>
  );
}

function RecentExecs({ wsId }: { wsId: string }) {
  const api = useApi();
  // Jails against this workspace are labelled `workspace:<id>/<cmd>`;
  // the ledger already supports substring search, so one filtered query
  // gets us the recent execs without a new endpoint.
  const { data } = useQuery({
    queryKey: ["jails", { q: `workspace:${wsId}`, kind: "workspace", limit: 8 }],
    queryFn:  () => api.jails.list({ q: `workspace:${wsId}`, kind: "workspace", limit: 8 }),
    refetchInterval: 3000,
  });

  const rows = data?.rows ?? [];
  return (
    <div>
      <div className="flex items-center justify-between mb-2">
        <div className="text-[10px] font-medium uppercase tracking-[0.2em] text-ink-400">
          Recent execs
        </div>
        <Link
          to={`/operator/ledger?q=${encodeURIComponent(`workspace:${wsId}`)}&kind=workspace`}
          className="text-[10px] mono text-ink-500 hover:text-ink-200"
        >see all →</Link>
      </div>
      {rows.length === 0 ? (
        <div className="text-[11.5px] text-ink-500 mono">no execs yet</div>
      ) : (
        <div className="grid gap-px bg-ink-800 rounded overflow-hidden">
          {rows.map((r) => <RecentExecRow key={r.id} rec={r} />)}
        </div>
      )}
    </div>
  );
}

function RecentExecRow({ rec }: { rec: JailRecord }) {
  // Labels look like `workspace:<id>/<cmd>` — drop the prefix for the
  // row so the cmd is the eye-catch.
  const cmd = rec.label.split("/").slice(1).join("/") || rec.label;
  const ok  = rec.status === "completed" && rec.exit_code === 0;
  const err = rec.status === "error" || (rec.status === "completed" && rec.exit_code !== 0);
  return (
    <Link
      to={`/operator/ledger?selected=${rec.id}`}
      className="bg-ink-900/60 px-3 py-2 grid grid-cols-[1fr_auto_auto] gap-3 items-center text-[11.5px] hover:bg-ink-900 transition-colors"
    >
      <span className="mono text-ink-200 truncate">{cmd}</span>
      <span className="mono text-ink-500">
        {rec.duration_ms != null ? humanMs(rec.duration_ms) : "…"}
      </span>
      <span
        className={cn(
          "mono text-[10px] uppercase tracking-wider",
          rec.status === "running" ? "text-phantom"
          : ok ? "text-ink-400"
          : err ? "text-[var(--color-siren)]"
          : "text-ink-500",
        )}
      >
        {rec.status === "running" ? "●" : (rec.exit_code != null ? `exit ${rec.exit_code}` : rec.status)}
        <span className="text-ink-600 ml-2">· {timeAgo(rec.started_at)}</span>
      </span>
    </Link>
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

function Row({
  label, value, span, tone,
}: {
  label: string;
  value: string;
  span?: boolean;
  tone?: "phantom" | "flare";
}) {
  const color = tone === "flare" ? "text-[var(--color-flare)]"
              : tone === "phantom" ? "text-[var(--color-phantom)]"
              : "text-ink-100";
  if (span) {
    return (
      <div className="text-[12px]">
        <div className="text-ink-500 mb-0.5">{label}</div>
        <div className={cn("mono break-all", color)}>{value}</div>
      </div>
    );
  }
  return (
    <div className="grid grid-cols-[90px_1fr] gap-3 items-baseline text-[12px]">
      <span className="text-ink-500">{label}</span>
      <span className={cn("mono break-all", color)}>{value}</span>
    </div>
  );
}

// ─── create form (unchanged shape) ──────────────────────────────────────

function CreateWorkspaceForm() {
  const api = useApi();
  const qc = useQueryClient();
  const [repo, setRepo] = useState("");
  const [ref, setRef] = useState("");
  const [label, setLabel] = useState("");
  const [idle, setIdle] = useState("");
  const [memory, setMemory] = useState("");
  const [selectedFlavors, setSelectedFlavors] = useState<string[]>([]);

  // Flavors are server-configured (dirs under $state_dir/flavors).
  // Refetch modestly — operators add a new flavor by dropping a dir
  // on the host; we don't need instant propagation.
  const { data: flavors } = useQuery({
    queryKey: ["flavors"],
    queryFn:  () => api.flavors.list(),
    staleTime: 30_000,
  });

  const create = useMutation({
    mutationFn: (req: WorkspaceCreateRequest) => api.workspaces.create(req),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["workspaces"] });
      setRepo("");
      setRef("");
      setLabel("");
      setIdle("");
      setMemory("");
      setSelectedFlavors([]);
    },
  });

  function toggleFlavor(name: string) {
    setSelectedFlavors((prev) =>
      prev.includes(name) ? prev.filter((f) => f !== name) : [...prev, name],
    );
  }

  function submit(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault();
    const req: WorkspaceCreateRequest = {};
    if (repo.trim())  req.git = { repo: repo.trim(), ...(ref.trim() ? { ref: ref.trim() } : {}) };
    if (label.trim()) req.label = label.trim();
    const idleN = parseInt(idle, 10);
    if (!Number.isNaN(idleN) && idleN > 0) req.idle_timeout_secs = idleN;
    const memN = parseInt(memory, 10);
    if (!Number.isNaN(memN) && memN > 0) req.memory_mb = memN;
    if (selectedFlavors.length > 0) req.flavors = selectedFlavors;
    create.mutate(req);
  }

  return (
    <Panel padded={false} className="flex-1 min-h-0 flex flex-col">
      <div className="px-5 py-4">
        <PanelHeader eyebrow="Create" title="New project" className="!mb-0" />
      </div>
      <div className="hairline" />
      <form onSubmit={submit} className="p-5 space-y-3 overflow-y-auto flex-1 min-h-0">
        <Field
          label="Git repo (optional)"
          placeholder="https://github.com/my-org/app"
          value={repo}
          onChange={(e: React.ChangeEvent<HTMLInputElement>) => setRepo(e.target.value)}
        />
        <Field
          label="Ref (optional)"
          placeholder="main"
          value={ref}
          onChange={(e: React.ChangeEvent<HTMLInputElement>) => setRef(e.target.value)}
        />
        <Field
          label="Label (optional)"
          placeholder="review-bot"
          value={label}
          onChange={(e: React.ChangeEvent<HTMLInputElement>) => setLabel(e.target.value)}
        />
        <div className="grid grid-cols-2 gap-3">
          <Field
            label="Auto-pause after (s)"
            placeholder="0"
            inputMode="numeric"
            value={idle}
            onChange={(e: React.ChangeEvent<HTMLInputElement>) => setIdle(e.target.value)}
          />
          <Field
            label="Memory (MB)"
            placeholder="512"
            inputMode="numeric"
            value={memory}
            onChange={(e: React.ChangeEvent<HTMLInputElement>) => setMemory(e.target.value)}
          />
        </div>
        {flavors && flavors.length > 0 && (
          <div>
            <div className="text-[10px] uppercase tracking-[0.18em] text-ink-400 mb-1.5">
              Runtime flavors
            </div>
            <div className="flex flex-wrap gap-1.5">
              {flavors.map((f) => {
                const on = selectedFlavors.includes(f.name);
                return (
                  <button
                    key={f.name}
                    type="button"
                    onClick={() => toggleFlavor(f.name)}
                    className={cn(
                      "mono text-[11px] rounded-full px-2.5 py-1 ring-1 transition-colors",
                      on
                        ? "bg-phantom/15 text-phantom ring-phantom/40"
                        : "bg-ink-900/40 text-ink-300 ring-ink-700 hover:bg-ink-800/60",
                    )}
                    aria-pressed={on}
                  >
                    {on ? "✓ " : ""}{f.name}
                  </button>
                );
              })}
            </div>
            <div className="text-[11px] text-ink-500 mt-1.5">
              Bind-mounted read-only into the jail at <code className="mono">/opt/flavors/&lt;name&gt;</code>; each flavor's <code className="mono">bin/</code> is prepended to <code className="mono">PATH</code>.
            </div>
          </div>
        )}
        <div className="pt-1">
          <Button type="submit" variant="primary" size="md" disabled={create.isPending}>
            {create.isPending ? "creating…" : "create project"}
          </Button>
        </div>
        {create.error && (
          <div className="text-[11.5px] text-[var(--color-siren)]">
            {create.error instanceof Error ? create.error.message : String(create.error)}
          </div>
        )}
      </form>
    </Panel>
  );
}

// ─── zero-state welcome ─────────────────────────────────────────────────

function ProjectsWelcome() {
  return (
    <div className="max-w-[720px] mx-auto pt-6">
      <div className="text-center mb-6">
        <div className="text-[10px] uppercase tracking-[0.22em] text-ink-400 mb-1.5">
          Projects
        </div>
        <h1 className="display text-[28px] leading-tight font-semibold text-balance">
          Create your first project
        </h1>
        <p className="mt-2 text-sm text-ink-400 max-w-[480px] mx-auto">
          A project is a persistent sandbox where your agent runs — clone a
          repo or start empty, run commands, snapshot the result. Nothing the
          agent does escapes the jail.
        </p>
      </div>
      <CreateWorkspaceForm />
      <p className="mt-4 text-center text-[11.5px] text-ink-500">
        Next up: <Link to="/sessions" className="text-ink-300 hover:text-ink-100 underline decoration-ink-700 underline-offset-2">mint an API session</Link> so the sandbox can call Anthropic, OpenAI, GitHub, or Stripe through the phantom proxy.
      </p>
    </div>
  );
}
