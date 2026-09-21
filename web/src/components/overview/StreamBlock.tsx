import { Link } from "react-router-dom";
import type { AuditRow } from "../../lib/api";
import { Panel, PanelHeader } from "../Panel";
import { Button } from "../Button";
import { AuditList } from "../AuditList";

export function StreamBlock({ rows }: { rows: AuditRow[] }) {
  return (
    <Panel padded={false}>
      <div className="px-5 py-4">
        <PanelHeader
          eyebrow="Stream"
          title="Recent phantom requests"
          action={
            <Link to="/operator/audit">
              <Button variant="ghost" size="sm">open stream →</Button>
            </Link>
          }
          className="!mb-0"
        />
      </div>
      <div className="hairline" />
      <div className="max-h-[340px] overflow-y-auto">
        <AuditList rows={rows} limit={12} />
      </div>
    </Panel>
  );
}
