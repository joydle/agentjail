import { Link } from "react-router-dom";
import type { Session } from "../../lib/api";
import { Panel, PanelHeader } from "../Panel";
import { Button } from "../Button";
import { ServiceStack } from "../ServiceBadge";
import { timeAgo } from "../../lib/format";

export function ActiveSessionsPanel({ sessions }: { sessions?: Session[] }) {
  const list = sessions ?? [];
  return (
    <Panel>
      <PanelHeader
        eyebrow="Active"
        title="Sessions"
        action={
          <Link to="/sessions">
            <Button variant="ghost" size="sm">all →</Button>
          </Link>
        }
      />
      {list.length === 0 ? (
        <div className="py-6 text-center text-xs text-ink-500 mono">
          no active sessions
        </div>
      ) : (
        <ul className="space-y-2">
          {list.slice(0, 6).map((s) => (
            <SessionRow key={s.id} session={s} />
          ))}
        </ul>
      )}
    </Panel>
  );
}

function SessionRow({ session }: { session: Session }) {
  return (
    <li className="flex items-center justify-between gap-2 h-10 px-3 rounded-lg ring-1 ring-ink-800 bg-ink-900/50">
      <div className="min-w-0 flex items-center gap-2">
        <span className="w-1.5 h-1.5 rounded-full bg-[var(--color-phantom)]" />
        <span className="mono text-[11px] text-ink-200 truncate">
          {session.id.slice(0, 12)}
        </span>
      </div>
      <div className="flex items-center gap-2">
        <ServiceStack services={session.services} size={16} />
        <span className="text-[10px] text-ink-500 mono whitespace-nowrap">
          {timeAgo(session.created_at)}
        </span>
      </div>
    </li>
  );
}
