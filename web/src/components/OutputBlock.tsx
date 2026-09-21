type Tone = "phantom" | "flare" | "siren";

const channel: Record<Tone, string> = {
  phantom: "var(--color-phantom)",
  flare:   "var(--color-flare)",
  siren:   "var(--color-siren)",
};

/** Labeled stream of text output — stdout, stderr, or error. */
export function OutputBlock({
  label,
  tone,
  text,
  showSize = true,
}: {
  label: string;
  tone: Tone;
  text: string;
  showSize?: boolean;
}) {
  return (
    <div>
      <div className="flex items-center gap-2 mb-1.5">
        <span
          className="text-[10px] uppercase tracking-[0.22em] font-medium"
          style={{ color: channel[tone] }}
        >
          ┃ {label}
        </span>
        <span className="flex-1 h-px" style={{ background: `${channel[tone]}22` }} />
        {showSize && <span className="text-[10px] text-ink-600 mono">{text.length}b</span>}
      </div>
      <pre
        className="mono text-[12px] text-ink-200 whitespace-pre-wrap break-words p-3 rounded-lg ring-1"
        style={{
          background:
            tone === "siren" ? "var(--color-siren-bg)" : "var(--color-ink-900)",
          borderColor: `${channel[tone]}22`,
        }}
      >
        {text || <span className="text-ink-600">(empty)</span>}
      </pre>
    </div>
  );
}
