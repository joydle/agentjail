/**
 * The "phantom flow" — a hero visualization of how a request travels:
 *
 *   sandbox  →  phantom  →  proxy  →  upstream
 *
 * Animated beads run along the wire at a cadence driven by the live
 * request rate. Pure SVG, zero dependencies.
 */
interface FlowProps {
  rate?: number;
}

const NODES = [
  { label: "SANDBOX",  sub: "jailed",     x: 40,  color: "#9B8CFF" },
  { label: "PHANTOM",  sub: "phm_...",    x: 170, color: "#7FFFBD" },
  { label: "PROXY",    sub: "host-local", x: 300, color: "#7FFFBD" },
  { label: "UPSTREAM", sub: "real key",   x: 430, color: "#FFB366" },
] as const;

export function Flow({ rate = 0 }: FlowProps) {
  const beadCount = Math.min(6, Math.max(0, Math.round(rate)));
  const beads = Array.from({ length: beadCount }, (_, i) => i);

  return (
    <div className="relative">
      <svg viewBox="0 0 480 120" className="w-full h-[130px]" preserveAspectRatio="xMidYMid meet">
        {/* wire */}
        <line
          x1="40" y1="60" x2="440" y2="60"
          stroke="var(--color-ink-600)"
          strokeWidth="1"
          strokeDasharray="2 4"
        />
        {/* glow underlay */}
        <line
          x1="40" y1="60" x2="440" y2="60"
          stroke="var(--color-phantom)"
          strokeWidth="0.6"
          opacity="0.35"
        />

        {/* beads traversing the wire */}
        {beads.map((i) => (
          <circle key={i} r="2.5" fill="var(--color-phantom)">
            <animate
              attributeName="cx"
              from="40"
              to="440"
              dur={`${2.4 + i * 0.15}s`}
              begin={`${(i * 2.4) / beadCount}s`}
              repeatCount="indefinite"
            />
            <animate
              attributeName="cy"
              values="60;60"
              dur="2.4s"
              repeatCount="indefinite"
            />
            <animate
              attributeName="opacity"
              values="0;1;1;0"
              keyTimes="0;0.1;0.9;1"
              dur={`${2.4 + i * 0.15}s`}
              begin={`${(i * 2.4) / beadCount}s`}
              repeatCount="indefinite"
            />
          </circle>
        ))}

        {/* nodes */}
        {NODES.map((n, i) => (
          <g key={n.label} transform={`translate(${n.x}, 60)`}>
            <circle r="14" fill="var(--color-ink-900)" stroke={n.color} strokeWidth="1.25" />
            <circle r="4" fill={n.color} opacity="0.9">
              <animate
                attributeName="opacity"
                values="0.5;1;0.5"
                dur="2.8s"
                begin={`${i * 0.35}s`}
                repeatCount="indefinite"
              />
            </circle>
            <text
              y="-24"
              textAnchor="middle"
              className="mono"
              fill="var(--color-ink-200)"
              fontSize="8.5"
              style={{ letterSpacing: "0.14em", fontWeight: 600 }}
            >
              {n.label}
            </text>
            <text
              y="30"
              textAnchor="middle"
              className="mono"
              fill="var(--color-ink-400)"
              fontSize="7.5"
            >
              {n.sub}
            </text>
          </g>
        ))}

        {/* seal between phantom and proxy */}
        <g transform="translate(235, 60)">
          <rect x="-8" y="-7" width="16" height="14" rx="3" fill="var(--color-ink-850)" stroke="var(--color-phantom)" strokeWidth="0.75" />
          <path d="M-3,-2 L-3,-4 Q0,-6 3,-4 L3,-2" fill="none" stroke="var(--color-phantom)" strokeWidth="0.75" />
        </g>
      </svg>
    </div>
  );
}
