import { Link } from "react-router-dom";
import { Logo } from "../components/Logo";
import { Pill } from "../components/Pill";
import { Button } from "../components/Button";
import { CodeShowcase } from "../components/landing/CodeShowcase";

/**
 * Public landing page. Conveys the phantom-edge pitch and cycles through
 * four real-world agent workloads with live syntax highlighting.
 */
export function Landing() {
  return (
    <div className="min-h-screen flex flex-col">
      <Nav />

      <main className="flex-1 max-w-[1360px] w-full mx-auto px-6 pt-12 pb-20">
        <div className="grid gap-10 items-start grid-cols-2">
          <Hero />
          <CodeShowcase />
        </div>

        <Pillars />
        <Bottom />
      </main>

      <Footer />
    </div>
  );
}

function Nav() {
  return (
    <header className="h-14 border-b border-ink-700/80 bg-ink-900/60 backdrop-blur-md sticky top-0 z-20">
      <div className="max-w-[1360px] mx-auto px-6 h-full flex items-center justify-between">
        <div className="flex items-center gap-3">
          <Logo size={24} />
          <div className="flex items-baseline gap-1.5">
            <span className="display text-[15px] font-semibold tracking-tight">agentjail</span>
            <span className="text-[11px] text-ink-400 mono">/ phantom edge</span>
          </div>
        </div>
        <nav className="flex items-center gap-4 text-[13px]">
          <a
            href="#pillars"
            className="text-ink-400 hover:text-ink-200 transition-colors hidden md:inline-block"
          >
            how it works
          </a>
          <Link
            to="/docs"
            className="text-ink-400 hover:text-ink-200 transition-colors"
          >
            docs
          </Link>
          <a
            href="https://github.com/bugthesystem/agentjail"
            target="_blank"
            rel="noreferrer"
            className="text-ink-400 hover:text-ink-200 transition-colors"
          >
            github ↗
          </a>
          <Link to="/login">
            <Button variant="primary" size="sm">sign in →</Button>
          </Link>
        </nav>
      </div>
    </header>
  );
}

function Hero() {
  return (
    <div className="pr-0 lg:pr-6">
      <Pill tone="phantom" dot className="mb-5">phantom tokens live</Pill>
      <h1 className="display text-[52px] leading-[1.02] font-semibold tracking-[-0.03em] text-balance">
        Sandboxes for agents that{" "}
        <span className="text-[var(--color-phantom)]">can&rsquo;t&nbsp;leak&nbsp;your&nbsp;keys.</span>
      </h1>
      <p className="mt-5 text-[15px] text-ink-300 leading-relaxed max-w-[540px]">
        A rootless Linux jail plus a phantom-token reverse proxy. Agents see{" "}
        <span className="mono text-ink-100">phm_…</span>, not{" "}
        <span className="mono text-ink-100">sk-…</span> — and the proxy swaps them at the
        edge so prompt injection and compromised packages have nothing to
        steal.
      </p>

      <div className="mt-7 flex items-center gap-3">
        <Link to="/login">
          <Button variant="primary" className="h-10 px-4">
            open dashboard →
          </Button>
        </Link>
        <a
          href="https://github.com/bugthesystem/agentjail"
          target="_blank"
          rel="noreferrer"
        >
          <Button variant="outline" className="h-10 px-4">github ↗</Button>
        </a>
      </div>

      <div className="mt-8 grid grid-cols-3 gap-4 text-[12px] mono">
        <StatBadge k="tests" v="17+19" hint="Rust · SDK" />
        <StatBadge k="bundle" v="317KB" hint="gzipped JS" />
        <StatBadge k="isolation" v="6 layers" hint="ns · seccomp · cgroups · landlock" />
      </div>
    </div>
  );
}

function StatBadge({ k, v, hint }: { k: string; v: string; hint: string }) {
  return (
    <div className="panel !p-3">
      <div className="flex items-baseline justify-between">
        <span className="text-[10px] uppercase tracking-[0.18em] text-ink-500">{k}</span>
        <span className="display text-lg text-ink-100 tabular-nums">{v}</span>
      </div>
      <div className="text-[10.5px] text-ink-500 mt-0.5 truncate">{hint}</div>
    </div>
  );
}

function Pillars() {
  const items: { glyph: string; title: string; body: string }[] = [
    {
      glyph: "◎",
      title: "Phantom edge",
      body:
        "Sandboxes only see per-session phm_ tokens. Our proxy resolves them to real keys, enforces scopes, and logs every request.",
    },
    {
      glyph: "✦",
      title: "Live fork",
      body:
        "Freeze a running jail for <1ms, COW-clone its output via FICLONE reflinks, and branch evaluation in parallel.",
    },
    {
      glyph: "◆",
      title: "Narrow networks",
      body:
        "Per-session network policy: none, loopback, or a domain allowlist routed through a host-local HTTP CONNECT proxy.",
    },
    {
      glyph: "◊",
      title: "Stream everything",
      body:
        "SSE streams stdout/stderr line-by-line; TypeScript SDK exposes it as an async iterator. No polling, no buffering.",
    },
  ];
  return (
    <section id="pillars" className="mt-16">
      <div className="text-[10px] uppercase tracking-[0.22em] text-ink-400 mb-2">
        Four primitives
      </div>
      <h2 className="display text-2xl font-semibold mb-6">
        Small surface, sharp edges
      </h2>
      <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-4 gap-4">
        {items.map((p, i) => (
          <div key={p.title} className="panel !p-5">
            <div
              className="text-2xl mb-3"
              style={{
                color: `var(--color-${["phantom", "flare", "iris", "siren"][i]})`,
              }}
            >
              {p.glyph}
            </div>
            <div className="display text-[15px] font-semibold text-ink-100">
              {p.title}
            </div>
            <p className="mt-1 text-[12.5px] text-ink-400 leading-relaxed">
              {p.body}
            </p>
          </div>
        ))}
      </div>
    </section>
  );
}

function Bottom() {
  return (
    <section className="mt-16 panel !p-8 text-center relative overflow-hidden">
      <div
        aria-hidden
        className="absolute inset-0 opacity-60 pointer-events-none"
        style={{
          background:
            "radial-gradient(ellipse at center, color-mix(in oklab, var(--color-phantom) 18%, transparent), transparent 70%)",
        }}
      />
      <h3 className="display text-2xl font-semibold relative">
        Already running the control plane?
      </h3>
      <p className="text-ink-400 mt-2 relative text-[13px]">
        Paste your endpoint + API key to open the dashboard.
      </p>
      <div className="mt-5 inline-flex gap-2 relative">
        <Link to="/login">
          <Button variant="primary" className="h-10 px-5">
            sign in →
          </Button>
        </Link>
      </div>
    </section>
  );
}

function Footer() {
  return (
    <footer className="border-t border-ink-800 py-8">
      <div className="max-w-[1360px] mx-auto px-6 flex items-center justify-between text-[11px] mono text-ink-500">
        <span>agentjail · open source · MIT</span>
        <span>phantom-edge preview</span>
      </div>
    </footer>
  );
}
