import { useState, type FormEvent } from "react";
import { useAuth } from "../lib/auth";
import { Logo } from "./Logo";
import { Field } from "./Input";
import { Button } from "./Button";
import { Flow } from "./Flow";

const DEFAULT_URL = "http://localhost:7070";

export function Login() {
  const { login, isLoading, error } = useAuth();
  const [baseUrl, setBaseUrl] = useState(DEFAULT_URL);
  const [apiKey, setApiKey] = useState("");

  async function onSubmit(e: FormEvent) {
    e.preventDefault();
    try {
      await login(baseUrl, apiKey);
    } catch {
      /* error set in context */
    }
  }

  return (
    <div className="h-full grid place-items-center px-6">
      <div className="w-full max-w-[960px] grid lg:grid-cols-[1fr_420px] gap-6">
        {/* left: brand panel */}
        <div className="panel p-10 relative overflow-hidden hidden lg:flex flex-col justify-between min-h-[520px]">
          <div className="absolute inset-0 grid-bg opacity-40" />
          <div
            className="absolute -top-20 -left-10 w-[360px] h-[360px] rounded-full blur-3xl opacity-60"
            style={{
              background:
                "radial-gradient(circle, color-mix(in oklab, var(--color-phantom) 35%, transparent), transparent 60%)",
            }}
          />
          <div className="relative">
            <div className="flex items-center gap-2.5 mb-8">
              <Logo size={28} />
              <span className="display text-lg font-semibold">agentjail</span>
            </div>
            <h1 className="display text-[40px] leading-[1.05] font-semibold text-balance">
              Give agents keys<br />
              they <span className="text-[var(--color-phantom)]">can&rsquo;t keep.</span>
            </h1>
            <p className="mt-4 text-sm text-ink-300 max-w-[360px] leading-relaxed">
              Phantom tokens swap in for real credentials at the proxy edge. The
              sandbox sees <span className="mono text-ink-200">phm_&hellip;</span>, never
              <span className="mono text-ink-200"> sk-&hellip;</span>
            </p>
          </div>

          <div className="relative mt-8">
            <Flow rate={0} />
          </div>
        </div>

        {/* right: auth form */}
        <div className="panel p-8 self-center">
          <div className="flex items-center gap-2 text-[10px] uppercase tracking-[0.25em] text-ink-400 mb-1">
            <span className="w-4 h-px bg-ink-500" />
            Connect
          </div>
          <h2 className="display text-2xl font-semibold mb-6">control plane</h2>

          <form className="space-y-4" onSubmit={onSubmit}>
            <Field
              label="Endpoint"
              value={baseUrl}
              onChange={(e) => setBaseUrl(e.target.value)}
              placeholder={DEFAULT_URL}
              prefix="url"
              autoComplete="url"
              required
            />
            <Field
              label="API Key"
              type="password"
              value={apiKey}
              onChange={(e) => setApiKey(e.target.value)}
              placeholder="aj_live_…"
              prefix="key"
              hint="leave empty if auth is disabled"
              error={error ?? undefined}
            />
            <Button
              type="submit"
              variant="primary"
              className="w-full justify-center h-10"
              disabled={isLoading}
            >
              {isLoading ? "connecting…" : "authenticate"}
              <span aria-hidden>↵</span>
            </Button>
          </form>

          <div className="mt-5 pt-4 border-t border-ink-700 text-[11px] text-ink-400 leading-relaxed">
            Defaults to your local docker compose stack at <span className="mono text-ink-300">:7070</span>.
          </div>
        </div>
      </div>
    </div>
  );
}
