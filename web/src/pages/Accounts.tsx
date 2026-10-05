import { Panel, PanelHeader } from "../components/Panel";
import { Pill } from "../components/Pill";
import { useWhoami } from "../lib/auth";

/**
 * Tenancy / identity page. Shows the caller's resolved scope and a
 * short explainer of how keys are provisioned today. The list of keys
 * themselves lives in server config (AGENTJAIL_API_KEY) — there's no
 * admin mutation surface yet, so this is intentionally read-only.
 *
 * When the key store becomes DB-backed, swap the "how keys are
 * configured" block for an admin-only CRUD panel.
 */
export function Accounts() {
  const me = useWhoami();

  return (
    <div className="grid gap-4 max-w-[820px]">
      <Panel padded={false}>
        <div className="px-5 py-4">
          <PanelHeader
            eyebrow="Identity"
            title="Your session"
            className="!mb-0"
          />
        </div>
        <div className="hairline" />
        <div className="p-5 grid gap-3">
          {me ? (
            <>
              <div className="grid grid-cols-[120px_1fr] gap-3 items-center text-[12.5px]">
                <span className="text-ink-400">Tenant</span>
                <span className="mono text-ink-100">{me.tenant}</span>
              </div>
              <div className="grid grid-cols-[120px_1fr] gap-3 items-center text-[12.5px]">
                <span className="text-ink-400">Role</span>
                <span>
                  <Pill tone={me.role === "admin" ? "phantom" : "ink"}>
                    {me.role}
                  </Pill>
                </span>
              </div>
              <div className="grid grid-cols-[120px_1fr] gap-3 items-baseline text-[12px] text-ink-500 mt-1">
                <span className="text-ink-400">Visibility</span>
                <span>
                  {me.role === "admin"
                    ? "Sees every tenant's workspaces, snapshots, and server settings."
                    : `Scoped to tenant ${me.tenant} — other tenants' rows are invisible.`}
                </span>
              </div>
            </>
          ) : (
            <span className="text-[12px] text-ink-500 mono">resolving…</span>
          )}
        </div>
      </Panel>

      <Panel padded={false}>
        <div className="px-5 py-4">
          <PanelHeader
            eyebrow="Keys"
            title="How accounts are provisioned"
            className="!mb-0"
          />
        </div>
        <div className="hairline" />
        <div className="p-5 grid gap-3 text-[12.5px] leading-6">
          <p className="text-ink-200">
            API keys live in the server's environment, one per line under{" "}
            <code className="mono text-phantom">AGENTJAIL_API_KEY</code>, in
            the form{" "}
            <code className="mono text-phantom">token@tenant:role</code>.
          </p>
          <pre className="mono text-[12px] text-ink-300 bg-ink-900 ring-1 ring-ink-800 rounded p-3 overflow-x-auto">
{`AGENTJAIL_API_KEY="
  ak_ops_9f3c@platform:admin,
  ak_acme_alice@acme:operator,
  ak_acme_bob@acme:operator
"`}
          </pre>
          <ul className="text-ink-400 grid gap-1.5 pl-4 list-disc marker:text-ink-600">
            <li>
              <span className="mono text-ink-200">admin</span> — unscoped.
              Sees every tenant's data; intended for the platform operator.
            </li>
            <li>
              <span className="mono text-ink-200">operator</span> —
              restricted to its tenant. Every list/get/patch/delete is
              filtered against the tenant id, every create stamps it onto
              the row.
            </li>
          </ul>
          <p className="text-ink-500 text-[11.5px] mt-2">
            There's no mutation UI yet — add / rotate / revoke is done by
            editing the env var and restarting the control plane. A
            DB-backed key store with CRUD endpoints is the next step.
          </p>
        </div>
      </Panel>
    </div>
  );
}
