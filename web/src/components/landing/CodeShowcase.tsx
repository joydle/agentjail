import { useEffect, useRef, useState } from "react";
import { CodeEditor } from "../CodeEditor";
import { SHOWCASES } from "../../lib/showcase";
import { cn } from "../../lib/cn";

const ROTATE_MS = 6500;

/**
 * Rotating stage of four "real-workload" code samples. Auto-advances every
 * ~6.5s; pauses on hover or when the user picks a tab manually.
 */
export function CodeShowcase() {
  const [idx, setIdx] = useState(0);
  const [paused, setPaused] = useState(false);
  const hoverRef = useRef(false);

  useEffect(() => {
    if (paused) return;
    const id = setInterval(() => {
      if (hoverRef.current) return;
      setIdx((i) => (i + 1) % SHOWCASES.length);
    }, ROTATE_MS);
    return () => clearInterval(id);
  }, [paused]);

  const current = SHOWCASES[idx];

  function pick(i: number) {
    setIdx(i);
    setPaused(true);
  }

  return (
    <div
      className="panel overflow-hidden flex flex-col"
      onMouseEnter={() => { hoverRef.current = true; }}
      onMouseLeave={() => { hoverRef.current = false; }}
    >
      {/* tab bar */}
      <div className="flex items-stretch border-b border-ink-800 px-2 py-2 gap-1 overflow-x-auto">
        {SHOWCASES.map((s, i) => {
          const on = i === idx;
          return (
            <button
              key={s.id}
              onClick={() => pick(i)}
              className={cn(
                "relative shrink-0 px-3 py-1.5 text-[12.5px] rounded-md transition-colors",
                on
                  ? "bg-ink-850 text-ink-100"
                  : "text-ink-400 hover:text-ink-200 hover:bg-ink-850/60",
              )}
            >
              <span>{s.label}</span>
              {on && (
                <span
                  className="absolute left-2 right-2 -bottom-[9px] h-px"
                  style={{
                    background:
                      "linear-gradient(90deg, transparent, var(--color-phantom), transparent)",
                  }}
                />
              )}
            </button>
          );
        })}
        <div className="flex-1" />
        <ProgressDots count={SHOWCASES.length} index={idx} paused={paused} />
      </div>

      {/* tag */}
      <div className="px-5 h-7 flex items-center border-b border-ink-800 text-[11px] mono text-ink-500">
        <span className="text-ink-300">&#9473;</span>
        <span className="ml-2 truncate">{current.tag}</span>
      </div>

      {/* code */}
      <div className="h-[420px] min-h-[420px]">
        <CodeEditor
          key={current.id}
          value={current.code}
          readOnly
          language={current.language}
        />
      </div>

      {/* footer: tiny "auto-advancing" hint or paused */}
      <div className="px-5 h-8 flex items-center justify-between border-t border-ink-800 text-[10.5px] mono text-ink-500">
        <span>
          {paused
            ? "paused · pick another tab to resume"
            : "auto-advancing · hover to pause"}
        </span>
        <span className="text-ink-600">
          {idx + 1} / {SHOWCASES.length}
        </span>
      </div>
    </div>
  );
}

function ProgressDots({
  count,
  index,
  paused,
}: {
  count: number;
  index: number;
  paused: boolean;
}) {
  return (
    <div className="flex items-center gap-1.5 px-2">
      {Array.from({ length: count }).map((_, i) => (
        <span
          key={i}
          className={cn(
            "w-1.5 h-1.5 rounded-full transition-colors",
            i === index
              ? "bg-[var(--color-phantom)]"
              : "bg-ink-700",
          )}
          style={{
            animation:
              i === index && !paused
                ? `pulse-ring 1.6s ease-in-out infinite`
                : undefined,
          }}
        />
      ))}
    </div>
  );
}
