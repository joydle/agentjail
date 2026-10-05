-- Tenancy: stamp every workspace, snapshot, and jail-ledger row with the
-- tenant that owns it.
--
-- Existing rows pre-date tenancy and all land under `"dev"` — the scope
-- the control plane emits when auth is disabled, so pre-upgrade data
-- stays reachable through the same dev configuration that produced it.
-- Prod deployments should rekey their rows to the real tenant ids via
-- an application-level migration after upgrading.

ALTER TABLE workspaces
    ADD COLUMN IF NOT EXISTS tenant_id text NOT NULL DEFAULT 'dev';

ALTER TABLE snapshots
    ADD COLUMN IF NOT EXISTS tenant_id text NOT NULL DEFAULT 'dev';

ALTER TABLE jails
    ADD COLUMN IF NOT EXISTS tenant_id text NOT NULL DEFAULT 'dev';

-- Indexes for the common filter shape: "rows owned by tenant T, newest
-- first" — serves the operator list views per resource.
CREATE INDEX IF NOT EXISTS workspaces_tenant_created_idx
    ON workspaces (tenant_id, created_at DESC)
    WHERE deleted_at IS NULL;

CREATE INDEX IF NOT EXISTS snapshots_tenant_created_idx
    ON snapshots (tenant_id, created_at DESC);

CREATE INDEX IF NOT EXISTS jails_tenant_id_idx
    ON jails (tenant_id, id DESC);
