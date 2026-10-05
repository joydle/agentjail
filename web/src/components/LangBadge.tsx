import { cn } from "../lib/cn";

export type Lang = "rust" | "ts" | "js" | "py" | "sh";

const META: Record<Lang, { label: string; file: string; color: string }> = {
  rust: { label: "Rust",       file: "rust.svg",       color: "#f0a06b" },
  ts:   { label: "TypeScript", file: "typescript.svg", color: "#3178c6" },
  js:   { label: "JavaScript", file: "javascript.svg", color: "#f7df1e" },
  py:   { label: "Python",     file: "python.svg",     color: "#3776ab" },
  sh:   { label: "Shell",      file: "gnubash.svg",    color: "#a8e3a8" },
};

/**
 * Tiny language pill — official simple-icons SVG masked with brand color
 * so users can scan the gallery and instantly see whether a snippet is the
 * Rust library (low-level, link-direct) or the TypeScript SDK (HTTP wrapper).
 */
export function LangBadge({ lang, size = 16, className }: { lang: Lang; size?: number; className?: string }) {
  const m = META[lang];
  const inner = Math.round(size * 0.72);
  return (
    <span
      title={m.label}
      aria-label={m.label}
      className={cn("inline-grid place-items-center rounded shrink-0 ring-1 ring-ink-900", className)}
      style={{ width: size, height: size, background: "var(--color-ink-900)" }}
    >
      <span
        style={{
          width: inner,
          height: inner,
          backgroundColor: m.color,
          WebkitMaskImage: `url(/lang/${m.file})`,
          maskImage: `url(/lang/${m.file})`,
          WebkitMaskRepeat: "no-repeat",
          maskRepeat: "no-repeat",
          WebkitMaskPosition: "center",
          maskPosition: "center",
          WebkitMaskSize: "contain",
          maskSize: "contain",
        }}
      />
    </span>
  );
}
