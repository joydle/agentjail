export type ServiceId = "openai" | "anthropic" | "github" | "stripe";

export const SERVICES: ServiceId[] = ["openai", "anthropic", "github", "stripe"];

export const SERVICE_META: Record<ServiceId, { label: string; accent: string; glyph: string }> = {
  openai:    { label: "OpenAI",    accent: "phantom", glyph: "◎" },
  anthropic: { label: "Anthropic", accent: "flare",   glyph: "✦" },
  github:    { label: "GitHub",    accent: "iris",    glyph: "◆" },
  stripe:    { label: "Stripe",    accent: "siren",   glyph: "◊" },
};

const rtf = new Intl.RelativeTimeFormat("en", { numeric: "auto" });

export function timeAgo(iso: string): string {
  const then = new Date(iso).getTime();
  const diff = Date.now() - then;
  const s = Math.round(diff / 1000);
  if (Math.abs(s) < 5) return "just now";
  const abs = Math.abs(s);
  const sign = s >= 0 ? -1 : 1; // rtf is "N units ago" when unit is negative
  if (abs < 60) return rtf.format(sign * abs, "second");
  const m = Math.round(abs / 60);
  if (m < 60) return rtf.format(sign * m, "minute");
  const h = Math.round(m / 60);
  if (h < 24) return rtf.format(sign * h, "hour");
  const d = Math.round(h / 24);
  return rtf.format(sign * d, "day");
}

export function clock(iso: string): string {
  const d = new Date(iso);
  return d.toLocaleTimeString("en", { hour12: false });
}

export function statusTone(status: number): "phantom" | "flare" | "siren" | "ink" {
  if (status === 0) return "ink";
  if (status >= 200 && status < 300) return "phantom";
  if (status >= 300 && status < 400) return "iris" as "ink";
  if (status >= 400 && status < 500) return "flare";
  return "siren";
}

export function truncate(s: string, max: number): string {
  if (s.length <= max) return s;
  return s.slice(0, max - 1) + "…";
}

export function humanBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`;
  return `${(n / 1024 / 1024 / 1024).toFixed(2)} GB`;
}

export function humanMs(n: number): string {
  if (n < 1000) return `${n}ms`;
  if (n < 60_000) return `${(n / 1000).toFixed(2)}s`;
  return `${(n / 60_000).toFixed(1)}m`;
}
