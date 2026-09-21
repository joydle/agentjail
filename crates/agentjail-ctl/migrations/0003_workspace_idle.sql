-- Idle-timeout auto-pause for persistent workspaces.
--
-- `last_exec_at` is bumped every time a workspace exec starts. A
-- background reaper scans workspaces where `last_exec_at + idle_timeout`
-- is in the past, takes an auto-snapshot, wipes the output dir, and
-- stores the snapshot id in `auto_snapshot`. The next exec against a
-- paused workspace auto-restores before running.
--
-- `idle_timeout_secs` lives inside `config` JSON (see WorkspaceSpec), so
-- this migration only adds the lifecycle columns.

ALTER TABLE workspaces ADD COLUMN IF NOT EXISTS last_exec_at   timestamptz;
ALTER TABLE workspaces ADD COLUMN IF NOT EXISTS paused_at      timestamptz;
ALTER TABLE workspaces ADD COLUMN IF NOT EXISTS auto_snapshot  text
    REFERENCES snapshots(id) ON DELETE SET NULL;

-- Reaper scans by (deleted_at IS NULL, paused_at IS NULL, last_exec_at < cutoff),
-- so cover the most selective column.
CREATE INDEX IF NOT EXISTS workspaces_idle_idx
    ON workspaces (last_exec_at) WHERE deleted_at IS NULL AND paused_at IS NULL;
