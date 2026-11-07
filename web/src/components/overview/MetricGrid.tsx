import { Metric } from "../Metric";
import { useSeries } from "../../lib/useSeries";

interface MetricGridProps {
  sessions: number;
  active: number;
  totalExecs: number;
  proxyEvents: number;
}

export function MetricGrid({ sessions, active, totalExecs, proxyEvents }: MetricGridProps) {
  const sessionSeries = useSeries(sessions);
  const activeSeries  = useSeries(active);
  const proxySeries   = useSeries(proxyEvents);

  return (
    <div className="grid grid-cols-3 gap-3">
      <Metric label="Active sessions" value={sessions}    tone="phantom" series={sessionSeries} />
      <Metric label="Running now"     value={active}      tone="flare"   series={activeSeries} delta={`${totalExecs} total`} />
      <Metric label="API calls seen"  value={proxyEvents} tone="iris"    series={proxySeries} />
    </div>
  );
}
