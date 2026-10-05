import { Link } from "react-router-dom";
import { Panel } from "../Panel";
import { Pill } from "../Pill";
import { Button } from "../Button";
import { Flow } from "../Flow";

interface HeroProps {
  totalEvents: number;
  rate: number;
  hasCreds: boolean;
  hasSessions: boolean;
  hasProjects: boolean;
}

export function Hero({
  totalEvents,
  rate,
  hasCreds,
  hasSessions,
  hasProjects,
}: HeroProps) {
  const zero = !hasCreds && !hasSessions && !hasProjects;

  return (
    <Panel className="!p-0 overflow-hidden">
      <div className="p-6 pb-4 flex items-start justify-between gap-6">
        <div className="min-w-0">
          <div className="text-[10px] uppercase tracking-[0.22em] text-ink-400 mb-1.5">
            {zero ? "Get started" : "Welcome back"}
          </div>
          <h1 className="display text-[28px] leading-tight font-semibold text-balance max-w-[520px]">
            {zero ? (
              <>Run agents behind a <span className="text-[var(--color-phantom)]">phantom edge</span></>
            ) : (
              <>What are you building today?</>
            )}
          </h1>
          <p className="mt-1.5 text-sm text-ink-400 max-w-[480px]">
            {zero
              ? <>Connect a service, spin up a project, and mint an API session — your sandbox only ever sees <span className="mono text-ink-300">phm_…</span> tokens.</>
              : <>Jump back into a project, mint a new session, or try a recipe in the playground.</>}
          </p>
        </div>
        <div className="flex flex-col items-end gap-2 shrink-0">
          <Pill tone="phantom" dot>proxy live</Pill>
          <div className="mono text-[11px] text-ink-500 tabular-nums">
            {totalEvents} requests seen
          </div>
        </div>
      </div>

      <div className="px-6 pb-4">
        {zero ? (
          <Checklist
            hasCreds={hasCreds}
            hasProjects={hasProjects}
            hasSessions={hasSessions}
          />
        ) : (
          <QuickActions />
        )}
      </div>

      <div className="px-3 pb-2 opacity-80">
        <Flow rate={rate} />
      </div>
    </Panel>
  );
}

function QuickActions() {
  return (
    <div className="flex flex-wrap gap-2">
      <Link to="/projects">
        <Button variant="primary" size="sm">+ New project</Button>
      </Link>
      <Link to="/sessions">
        <Button variant="outline" size="sm">Mint a session</Button>
      </Link>
      <Link to="/playground">
        <Button variant="ghost" size="sm">Open playground →</Button>
      </Link>
    </div>
  );
}

interface ChecklistProps {
  hasCreds: boolean;
  hasProjects: boolean;
  hasSessions: boolean;
}

function Checklist({ hasCreds, hasProjects, hasSessions }: ChecklistProps) {
  const steps = [
    {
      done: hasCreds,
      title: "Connect a service",
      hint: "Attach an OpenAI, Anthropic, GitHub, or Stripe key.",
      cta: "Integrations",
      to: "/integrations",
    },
    {
      done: hasProjects,
      title: "Create a project",
      hint: "A persistent sandbox where your agent runs.",
      cta: "Projects",
      to: "/projects",
    },
    {
      done: hasSessions,
      title: "Mint an API session",
      hint: "Get phantom tokens scoped to a single agent run.",
      cta: "Sessions",
      to: "/sessions",
    },
  ] as const;

  return (
    <ol className="grid gap-2 sm:grid-cols-3">
      {steps.map((s, i) => (
        <li
          key={s.title}
          className="rounded-lg ring-1 ring-ink-800 bg-ink-900/50 p-3 flex flex-col gap-1.5"
        >
          <div className="flex items-center gap-2">
            <span
              className={
                s.done
                  ? "w-4 h-4 rounded-full bg-[var(--color-phantom)] text-ink-950 text-[10px] font-bold flex items-center justify-center"
                  : "w-4 h-4 rounded-full ring-1 ring-ink-600 text-ink-400 text-[10px] font-medium flex items-center justify-center mono"
              }
              aria-hidden
            >
              {s.done ? "✓" : i + 1}
            </span>
            <span className="text-[13px] font-medium text-ink-100">{s.title}</span>
          </div>
          <p className="text-[11.5px] text-ink-400 leading-snug">{s.hint}</p>
          <Link to={s.to} className="mt-auto">
            <Button variant={s.done ? "ghost" : "outline"} size="sm">
              {s.done ? `View ${s.cta.toLowerCase()} →` : `${s.cta} →`}
            </Button>
          </Link>
        </li>
      ))}
    </ol>
  );
}
