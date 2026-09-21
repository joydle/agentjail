import { useQuery } from "@tanstack/react-query";
import { useApi, useIsAdmin } from "../lib/auth";
import { Panel, PanelHeader } from "../components/Panel";
import type { SettingsSnapshot } from "../lib/api";

/**
 * Read-only view of the running server's configuration. Three panels:
 * proxy (upstream providers + bind), jail defaults + concurrency, and
 * persistence + snapshot policy. Credentials never appear here — the
 * backend strips them before responding.
 */
export function Settings() {
  const api = useApi();
  const { data, isLoading, error } = useQuery({
    queryKey: ["settings"],
    queryFn: () => api.settings.get(),
  });

  if (isLoading) return <EmptyState label="loading" />;
  if (error)     return <EmptyState label={`error · ${error instanceof Error ? error.message : "unknown"}`} />;
  if (!data)     return <EmptyState label="no settings" />;

  return (
    <div className="grid gap-4">
      <ProxyPanel s={data} />

      <div className="grid gap-4 grid-cols-2">
        <JailDefaultsPanel s={data} />
        <PersistencePanel s={data} />
      </div>
    </div>
  );
}

function ProxyPanel({ s }: { s: SettingsSnapshot }) {
  return (
    <Panel padded={false}>
      <div className="px-5 py-4">
        <PanelHeader
          eyebrow="Proxy"
          title="Phantom-token reverse proxy"
          action={<span className="text-[11px] mono text-ink-500">{s.proxy.providers.length} providers</span>}
          className="!mb-0"
        />
      </div>
      <div className="hairline" />
      <div className="p-5 grid gap-4">
        <Row label="Sandbox base URL" value={s.proxy.base_url} />
        {/* Bind addresses + gateway route are admin-only; the server
            omits them for operator-role scopes. Render nothing rather
            than a placeholder so the UI doesn't imply "we hid something". */}
        {s.proxy.bind_addr      && <Row label="Bind"          value={s.proxy.bind_addr} />}
        {s.gateway?.bind_addr   && <Row label="Gateway bind"  value={s.gateway.bind_addr} />}
        {s.control_plane.bind_addr && <Row label="Control plane" value={s.control_plane.bind_addr} />}

        <div>
          <div className="text-[10px] font-medium uppercase tracking-[0.2em] text-ink-400 mb-2">
            Providers
          </div>
          <div className="grid gap-px bg-ink-800 rounded overflow-hidden">
            {s.proxy.providers.map((p) => (
              <div key={p.service_id} className="grid grid-cols-[120px_1fr_1fr] gap-3 px-3 py-2 bg-ink-900/60 text-[12px]">
                <span className="mono text-phantom">{p.service_id}</span>
                <span className="mono text-ink-300 truncate">{p.upstream_base}</span>
                <span className="mono text-ink-400 truncate">{p.request_prefix}</span>
              </div>
            ))}
            {s.proxy.providers.length === 0 && (
              <div className="px-3 py-2 bg-ink-900/60 text-[12px] text-ink-500">
                no providers registered
              </div>
            )}
          </div>
        </div>
      </div>
    </Panel>
  );
}

function JailDefaultsPanel({ s }: { s: SettingsSnapshot }) {
  return (
    <Panel padded={false}>
      <div className="px-5 py-4">
        <PanelHeader eyebrow="Jails" title="Exec defaults" className="!mb-0" />
      </div>
      <div className="hairline" />
      <div className="p-5 grid gap-3">
        {s.exec ? (
          <>
            <Row label="Default memory"  value={`${s.exec.default_memory_mb} MB`} />
            <Row label="Default timeout" value={`${s.exec.default_timeout_secs} s`} />
            <Row label="Max concurrent"  value={String(s.exec.max_concurrent)} />
          </>
        ) : (
          <span className="text-[12px] text-ink-500">
            exec disabled — <code className="mono">/v1/runs</code> + <code className="mono">/v1/sessions/:id/exec</code> return 501
          </span>
        )}
      </div>
    </Panel>
  );
}

function PersistencePanel({ s }: { s: SettingsSnapshot }) {
  return (
    <Panel padded={false}>
      <div className="px-5 py-4">
        <PanelHeader eyebrow="Persistence" title="State + snapshots" className="!mb-0" />
      </div>
      <div className="hairline" />
      <div className="p-5 grid gap-3">
        {/* Host paths are admin-only — dashboards/screenshots shouldn't
            teach viewers the daemon's on-disk layout. */}
        {s.persistence.state_dir && (
          <Row label="State dir" value={s.persistence.state_dir} />
        )}
        {s.persistence.snapshot_pool_dir !== undefined && (
          <Row label="Pool dir"
               value={s.persistence.snapshot_pool_dir ?? "— (full-copy snapshots)"} />
        )}
        <Row label="Idle reaper"  value={s.persistence.idle_check_secs === 0 ? "disabled" : `every ${s.persistence.idle_check_secs} s`} />
        {s.snapshots.gc ? (
          <>
            <Row label="GC max age" value={s.snapshots.gc.max_age_secs == null ? "∞" : `${s.snapshots.gc.max_age_secs} s`} />
            <Row label="GC max count" value={s.snapshots.gc.max_count == null ? "∞" : String(s.snapshots.gc.max_count)} />
            <Row label="GC tick"    value={`every ${s.snapshots.gc.tick_secs} s`} />
          </>
        ) : (
          <Row label="Snapshot GC" value="disabled" />
        )}
      </div>
    </Panel>
  );
}

function Row({ label, value }: { label: string; value: string }) {
  return (
    <div className="grid grid-cols-[140px_1fr] gap-3 items-baseline text-[12px]">
      <span className="text-ink-400">{label}</span>
      <span className="mono text-ink-100 break-all">{value}</span>
    </div>
  );
}

function EmptyState({ label }: { label: string }) {
  return (
    <Panel>
      <div className="text-[13px] text-ink-400 mono">{label}</div>
    </Panel>
  );
}
