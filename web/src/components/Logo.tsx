import { cn } from "../lib/cn";

/**
 * Inline `{∅}` mark for the header and other in-app chrome. The boxed /
 * gridded version used as the favicon + hero lives in
 * `web/public/logo.svg` and `/logo.svg` at the repo root; this one is
 * the glyph-only cut so it sits cleanly against the existing dark
 * background.
 */
export function Logo({ size = 28, className }: { size?: number; className?: string }) {
  const stroke = "var(--color-phantom)";
  return (
    <svg
      viewBox="0 0 128 128"
      width={size}
      height={size}
      className={cn("shrink-0", className)}
      aria-label="agentjail"
      fill="none"
    >
      <path
        d="M40 30 Q30 30 30 40 L30 58 Q30 64 22 64 Q30 64 30 70 L30 88 Q30 98 40 98"
        stroke={stroke}
        strokeWidth="4.5"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
      <path
        d="M88 30 Q98 30 98 40 L98 58 Q98 64 106 64 Q98 64 98 70 L98 88 Q98 98 88 98"
        stroke={stroke}
        strokeWidth="4.5"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
      <circle cx="64" cy="64" r="14" stroke={stroke} strokeWidth="3.5" />
      <line x1="52" y1="78" x2="76" y2="50" stroke={stroke} strokeWidth="3.5" strokeLinecap="round" />
    </svg>
  );
}
