import type { ReactNode } from "react";
import { useEffect } from "react";
import { Pill } from "../Pill";

export function DocPage({
  eyebrow,
  title,
  lead,
  children,
}: {
  eyebrow: string;
  title: string;
  lead?: ReactNode;
  children: ReactNode;
}) {
  useEffect(() => {
    document.title = `${title} · agentjail docs`;
  }, [title]);
  return (
    <article className="max-w-[760px]">
      <Pill tone="phantom" className="mb-3">{eyebrow}</Pill>
      <h1 className="display text-[40px] leading-[1.05] font-semibold tracking-[-0.025em] mb-4">
        {title}
      </h1>
      {lead && (
        <p className="text-[15.5px] text-ink-300 leading-relaxed mb-10 max-w-[640px]">
          {lead}
        </p>
      )}
      <div className="prose-doc">{children}</div>
    </article>
  );
}

export function Section({
  id,
  title,
  children,
}: {
  id: string;
  title: string;
  children: ReactNode;
}) {
  return (
    <section id={id} className="mt-10 first:mt-0 scroll-mt-20">
      <h2 className="display text-[22px] font-semibold tracking-[-0.02em] text-ink-100 mb-3">
        <a href={`#${id}`} className="group inline-flex items-baseline gap-2">
          {title}
          <span className="text-ink-600 group-hover:text-ink-400 transition-colors text-[15px]">
            #
          </span>
        </a>
      </h2>
      <div className="text-[14.5px] text-ink-300 leading-relaxed space-y-4">{children}</div>
    </section>
  );
}

export function P({ children }: { children: ReactNode }) {
  return <p>{children}</p>;
}

export function Inline({ children }: { children: ReactNode }) {
  return (
    <code className="mono text-[12.5px] px-1.5 py-0.5 rounded bg-ink-850 ring-1 ring-ink-700 text-ink-100">
      {children}
    </code>
  );
}

export function Hint({
  tone = "phantom",
  title,
  children,
}: {
  tone?: "phantom" | "flare" | "siren" | "iris";
  title?: string;
  children: ReactNode;
}) {
  return (
    <aside
      className="my-5 rounded-lg p-4 ring-1 text-[13.5px]"
      style={{
        background: `var(--color-${tone}-bg)`,
        borderColor: `var(--color-${tone})`,
        color: "var(--color-ink-200)",
        // @ts-expect-error CSS var
        "--ring-color": `var(--color-${tone})`,
      }}
    >
      {title && (
        <div className="display font-semibold text-[13px] mb-1" style={{ color: `var(--color-${tone})` }}>
          {title}
        </div>
      )}
      <div className="leading-relaxed">{children}</div>
    </aside>
  );
}

export function Cols({ children }: { children: ReactNode }) {
  return (
    <div className="my-5 grid grid-cols-1 md:grid-cols-2 gap-3">{children}</div>
  );
}

export function Card({
  title,
  children,
  href,
  to,
}: {
  title: string;
  children: ReactNode;
  href?: string;
  to?: string;
}) {
  const content = (
    <>
      <div className="display text-[14px] font-semibold text-ink-100 mb-1">{title}</div>
      <div className="text-[12.5px] text-ink-400 leading-relaxed">{children}</div>
    </>
  );
  const cls =
    "block panel !p-4 transition-colors hover:bg-ink-850/80 hover:ring-ink-600";
  if (href) return <a className={cls} href={href} target="_blank" rel="noreferrer">{content}</a>;
  if (to)   return <a className={cls} href={to}>{content}</a>;
  return <div className={cls}>{content}</div>;
}

export function List({ children }: { children: ReactNode }) {
  return (
    <ul className="list-disc pl-5 marker:text-ink-600 space-y-1.5 text-[14px] text-ink-300">
      {children}
    </ul>
  );
}

export function Table({
  head,
  rows,
}: {
  head: string[];
  rows: (ReactNode | string)[][];
}) {
  return (
    <div className="my-5 panel !p-0 overflow-hidden">
      <table className="w-full text-[12.5px]">
        <thead>
          <tr className="text-[10px] uppercase tracking-[0.18em] text-ink-500">
            {head.map((h) => (
              <th key={h} className="text-left px-4 py-3 font-medium border-b border-ink-700/70">
                {h}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {rows.map((r, i) => (
            <tr key={i} className="text-ink-300 border-b border-ink-800/60 last:border-b-0">
              {r.map((c, j) => (
                <td key={j} className="px-4 py-2.5 align-top">{c}</td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
