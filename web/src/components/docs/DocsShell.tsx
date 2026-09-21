import { NavLink, Outlet, Link } from "react-router-dom";
import { Logo } from "../Logo";
import { cn } from "../../lib/cn";
import { useAuth } from "../../lib/auth";

const NAV: { section: string; items: { to: string; label: string; hint?: string }[] }[] = [
  {
    section: "Start",
    items: [
      { to: "quickstart", label: "Quickstart",       hint: "5-minute setup" },
    ],
  },
  {
    section: "Build",
    items: [
      { to: "sdk",        label: "TypeScript SDK",   hint: "@agentjail/sdk" },
      { to: "playground", label: "Playground",       hint: "in-browser recipes" },
    ],
  },
  {
    section: "Concepts",
    items: [
      { to: "phantom",    label: "Phantom proxy",    hint: "credential edge" },
      { to: "network",    label: "Network modes",    hint: "none · loopback · allowlist" },
      { to: "forking",    label: "Live forking",     hint: "COW jail clones" },
      { to: "workspaces", label: "Workspaces",       hint: "persistent multi-exec" },
      { to: "snapshots",  label: "Snapshots",        hint: "freeze & restore" },
      { to: "gateway",    label: "Gateway",          hint: "hostname → backend URL" },
    ],
  },
  {
    section: "Reference",
    items: [
      { to: "security",   label: "Security model",   hint: "6 isolation layers" },
    ],
  },
];

export function DocsShell() {
  const { auth } = useAuth();

  return (
    <div className="min-h-screen flex flex-col">
      <header className="h-14 border-b border-ink-700/80 bg-ink-900/60 backdrop-blur-md sticky top-0 z-20">
        <div className="max-w-[1280px] mx-auto px-6 h-full flex items-center justify-between">
          <Link to="/" className="flex items-center gap-3">
            <Logo size={22} />
            <div className="flex items-baseline gap-1.5">
              <span className="display text-[15px] font-semibold tracking-tight">agentjail</span>
              <span className="text-[11px] text-ink-400 mono">/ docs</span>
            </div>
          </Link>
          <nav className="flex items-center gap-4 text-[13px]">
            <a
              href="https://github.com/bugthesystem/agentjail"
              target="_blank" rel="noreferrer"
              className="text-ink-400 hover:text-ink-200 transition-colors"
            >
              github ↗
            </a>
            {auth ? (
              <Link to="/" className="text-ink-400 hover:text-ink-200 transition-colors">dashboard →</Link>
            ) : (
              <Link to="/login" className="text-ink-400 hover:text-ink-200 transition-colors">sign in →</Link>
            )}
          </nav>
        </div>
      </header>

      <div className="max-w-[1280px] w-full mx-auto px-6 py-10 grid gap-10 grid-cols-[220px_minmax(0,1fr)]">
        <aside className="text-[13px]">
          {NAV.map((s) => (
            <div key={s.section} className="mb-6">
              <div className="text-[10px] uppercase tracking-[0.22em] text-ink-500 mb-2 font-medium">
                {s.section}
              </div>
              <ul className="space-y-0.5">
                {s.items.map((it) => (
                  <li key={it.to}>
                    <NavLink
                      to={it.to}
                      className={({ isActive }) =>
                        cn(
                          "block rounded-md px-2.5 py-1.5 transition-colors",
                          isActive
                            ? "bg-[var(--color-phantom-bg)] text-ink-100 ring-1 ring-[var(--color-phantom)]/30"
                            : "text-ink-300 hover:text-ink-100 hover:bg-ink-850/60",
                        )
                      }
                    >
                      <div>{it.label}</div>
                      {it.hint && (
                        <div className="text-[10.5px] text-ink-500 leading-tight">{it.hint}</div>
                      )}
                    </NavLink>
                  </li>
                ))}
              </ul>
            </div>
          ))}
        </aside>

        <main className="min-w-0">
          <Outlet />
        </main>
      </div>
    </div>
  );
}
