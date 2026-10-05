/**
 * Minimal SVG sparkline. Accepts a numeric series and renders as a filled path.
 * No axes, no labels — purely ambient.
 */
interface SparklineProps {
  data: number[];
  stroke?: string;
  height?: number;
  className?: string;
}

export function Sparkline({ data, stroke = "var(--color-phantom)", height = 36, className }: SparklineProps) {
  if (data.length < 2) {
    return <div style={{ height }} className={className} />;
  }
  const w = 100;
  const h = 30;
  const max = Math.max(...data, 1);
  const min = Math.min(...data, 0);
  const range = max - min || 1;
  const step = w / (data.length - 1);
  const points = data.map((v, i) => {
    const x = i * step;
    const y = h - ((v - min) / range) * h;
    return [x, y] as const;
  });

  const linePath = points
    .map(([x, y], i) => (i === 0 ? `M${x.toFixed(2)},${y.toFixed(2)}` : `L${x.toFixed(2)},${y.toFixed(2)}`))
    .join(" ");

  const fillPath = `${linePath} L${w},${h} L0,${h} Z`;
  const last = points[points.length - 1];

  return (
    <svg viewBox={`0 0 ${w} ${h}`} preserveAspectRatio="none" style={{ height }} className={className}>
      <defs>
        <linearGradient id={`spark-${stroke}`} x1="0" y1="0" x2="0" y2="1">
          <stop offset="0%" stopColor={stroke} stopOpacity="0.35" />
          <stop offset="100%" stopColor={stroke} stopOpacity="0" />
        </linearGradient>
      </defs>
      <path d={fillPath} fill={`url(#spark-${stroke})`} />
      <path d={linePath} fill="none" stroke={stroke} strokeWidth="1.25" vectorEffect="non-scaling-stroke" />
      <circle cx={last[0]} cy={last[1]} r="1.6" fill={stroke} />
    </svg>
  );
}
