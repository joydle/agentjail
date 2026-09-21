import { useEffect, useRef, useState } from "react";

/**
 * Rolling buffer of *observed* values. Starts empty — no synthetic history.
 * Push the current live `value` every `tickMs`; keep only the last `window`.
 * Consumers should treat < 2 samples as "not enough yet" and hide the viz.
 */
export function useSeries(value: number, window = 30, tickMs = 1500): number[] {
  const [series, setSeries] = useState<number[]>([]);
  const ref = useRef(value);
  useEffect(() => { ref.current = value; }, [value]);
  useEffect(() => {
    const id = setInterval(() => {
      setSeries((s) => {
        const next = [...s, ref.current];
        return next.length > window ? next.slice(-window) : next;
      });
    }, tickMs);
    return () => clearInterval(id);
  }, [window, tickMs]);
  return series;
}
