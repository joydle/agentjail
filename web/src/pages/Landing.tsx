import { Link } from "react-router-dom";
import { Logo } from "../components/Logo";
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
            <span className="text-[11px] text-ink-400 mono">/ control plane</span>
          </div>
        </div>
        <nav className="flex items-center gap-4 text-[13px]">
          <a
            href="#pillars"
            className="text-ink-400 hover:text-ink-200 transition-colors hidden md:inline-block"
          >
            how it works
          </a>
          <a
            href="https://github.com/joydle/agentjail/blob/main/docs/control-plane.md"
            target="_blank"
            rel="noreferrer"
            className="text-ink-400 hover:text-ink-200 transition-colors"
          >
            docs ↗
          </a>
          <a
            href="https://github.com/joydle/agentjail"
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
      <h1 className="display text-[52px] leading-[1.02] font-semibold tracking-[-0.03em] text-balance">
        Linux sandboxes with{" "}
        <span className="text-[var(--color-phantom)]">phantom&nbsp;credentials.</span>
      </h1>
      <p className="mt-5 text-[15px] text-ink-300 leading-relaxed max-w-[540px]">
        Jails run untrusted code inside namespaces, seccomp, Landlock and
        cgroup limits. A session hands the sandbox{" "}
        <span className="mono text-ink-100">phm_…</span> tokens instead of{" "}
        <span className="mono text-ink-100">sk-…</span> keys; the proxy
        swaps in the real key on the way out, within the session&rsquo;s
        scopes and lifetime.
      </p>

      <div className="mt-7 flex items-center gap-3">
        <Link to="/login">
          <Button variant="primary" className="h-10 px-4">
            open dashboard →
          </Button>
        </Link>
        <a
          href="https://github.com/joydle/agentjail"
          target="_blank"
          rel="noreferrer"
        >
          <Button variant="outline" className="h-10 px-4">github ↗</Button>
        </a>
      </div>

    </div>
  );
}

function Pillars() {
  const items: { glyph: string; title: string; body: string }[] = [
    {
      glyph: "◎",
      title: "Phantom credentials",
      body:
        "Sandboxes only see per-session phm_ tokens. The proxy resolves them to real keys, enforces path scopes, and logs every request.",
    },
    {
      glyph: "✦",
      title: "Live fork",
      body:
        "Freeze a running jail, clone its output with FICLONE reflinks where the filesystem supports them, and run variants in parallel.",
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
        "Runs stream stdout and stderr line by line over SSE; the SDKs expose it as an iterator.",
    },
  ];
  return (
    <section id="pillars" className="mt-16">
      <div className="text-[10px] uppercase tracking-[0.22em] text-ink-400 mb-2">
        Four primitives
      </div>
      <h2 className="display text-2xl font-semibold mb-6">
        What it does
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
        <span>beta</span>
      </div>
    </footer>
  );
}
