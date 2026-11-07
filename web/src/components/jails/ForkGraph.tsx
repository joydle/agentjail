import { useQuery } from "@tanstack/react-query";
import type { JailRecord } from "../../lib/api";
import { useApi } from "../../lib/auth";
import { Panel, PanelHeader } from "../Panel";
import { Pill } from "../Pill";
import { humanBytes, humanMs } from "../../lib/format";
import { cn } from "../../lib/cn";

/**
 * Compact parent → child visualization for fork jails. Given a record,
 * resolves the sibling via `parent_id` and renders both nodes connected
 * by an SVG wire — the same idiom as the Flow component on the overview.
 */
export function ForkGraph({
  rec,
  onSelect,
}: {
  rec: JailRecord;
  onSelect?: (id: number) => void;
}) {
  const api = useApi();

  const partnerId = rec.parent_id ?? rec.id;
  const { data: parent } = useQuery({
    queryKey: ["jail", "parent", partnerId],
    queryFn:  () => api.jails.get(partnerId),
    enabled:  rec.parent_id != null,
  });

  const { data: siblings } = useQuery({
    queryKey: ["jail", "children", rec.id, rec.parent_id],
    queryFn:  () => api.jails.list({ limit: 200 }),
    enabled:  true,
  });

  const root: JailRecord = rec.parent_id != null ? (parent ?? rec) : rec;
  const children: JailRecord[] = (siblings?.rows ?? []).filter(
    (r) => r.parent_id === root.id,
  );

  if (children.length === 0 && rec.parent_id == null) return null;

  return (
    <Panel padded={false}>
      <div className="px-5 py-3 flex items-center justify-between">
        <PanelHeader eyebrow="Fork graph" title="Live-fork lineage" className="!mb-0" />
        <Pill tone="flare">{children.length + 1} jails</Pill>
      </div>
      <div className="hairline" />
      <div className="p-5">
        <Graph
          root={root}
          children={children}
          highlightId={rec.id}
          onSelect={onSelect}
        />
      </div>
    </Panel>
  );
}

function Graph({
  root,
  children,
  highlightId,
  onSelect,
}: {
  root: JailRecord;
  children: JailRecord[];
  highlightId: number;
  onSelect?: (id: number) => void;
}) {
  // Layout: root at the top, children fanned out below connected by curves.
  const n = children.length;
  const width = 520;
  const rootX = width / 2;
  const rootY = 42;
  const childY = 140;
  const spread = Math.min(480, n * 160);
  const childXs =
    n === 0
      ? []
      : n === 1
      ? [rootX]
      : Array.from({ length: n }, (_, i) => rootX - spread / 2 + (i * spread) / (n - 1));

  return (
    <svg viewBox={`0 0 ${width} 200`} className="w-full h-[200px]">
      {/* wires */}
      {children.map((c, i) => {
        const x = childXs[i];
        return (
          <path
            key={c.id}
            d={`M${rootX},${rootY + 20} C${rootX},${(rootY + childY) / 2} ${x},${(rootY + childY) / 2} ${x},${childY - 20}`}
            fill="none"
            stroke="var(--color-phantom)"
            strokeWidth="0.9"
            opacity="0.6"
          />
        );
      })}
      {/* root */}
      <Node
        x={rootX}
        y={rootY}
        rec={root}
        tag="parent"
        highlight={root.id === highlightId}
        onSelect={onSelect}
      />
      {/* children */}
      {children.map((c, i) => (
        <Node
          key={c.id}
          x={childXs[i]}
          y={childY}
          rec={c}
          tag="child"
          highlight={c.id === highlightId}
          onSelect={onSelect}
        />
      ))}
    </svg>
  );
}

function Node({
  x,
  y,
  rec,
  tag,
  highlight,
  onSelect,
}: {
  x: number;
  y: number;
  rec: JailRecord;
  tag: string;
  highlight: boolean;
  onSelect?: (id: number) => void;
}) {
  const tone =
    rec.status === "running"
      ? "var(--color-iris)"
      : rec.status === "error" || rec.timed_out || rec.oom_killed || (rec.exit_code !== null && rec.exit_code !== 0)
      ? "var(--color-siren)"
      : "var(--color-phantom)";

  return (
    <g
      transform={`translate(${x}, ${y})`}
      onClick={onSelect ? () => onSelect(rec.id) : undefined}
      className={cn(onSelect && "cursor-pointer")}
      role={onSelect ? "button" : undefined}
      tabIndex={onSelect ? 0 : undefined}
    >
      <title>jail #{rec.id} · {tag}</title>
      <rect
        x={-70}
        y={-18}
        width={140}
        height={40}
        rx={8}
        fill="var(--color-ink-900)"
        stroke={tone}
        strokeWidth={highlight ? 1.6 : 0.9}
        className={cn(
          "transition-all",
          highlight && "drop-shadow-[0_0_12px_var(--color-phantom)]",
          onSelect && "hover:brightness-125",
        )}
      />
      <text
        y={-3}
        textAnchor="middle"
        className="mono"
        fill="var(--color-ink-400)"
        fontSize="8.5"
        style={{ letterSpacing: "0.2em" }}
      >
        {tag.toUpperCase()} · #{rec.id}
      </text>
      <text
        y={10}
        textAnchor="middle"
        className="mono"
        fill={tone}
        fontSize="9.5"
      >
        {rec.memory_peak_bytes != null ? humanBytes(rec.memory_peak_bytes) : "—"}
        {rec.duration_ms != null ? ` · ${humanMs(rec.duration_ms)}` : ""}
      </text>
    </g>
  );
}
