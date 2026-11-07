-- Persistent workspaces + named snapshots.
--
-- A *workspace* is a long-lived mount tree (source + output) that survives
-- across HTTP requests. Each `POST /v1/workspaces/:id/exec` spawns a fresh
-- jail against the same dirs, so mutations persist between calls — the
-- "persistent VM" pattern expressed as a control-plane primitive.
--
-- A *snapshot* is a COW/copy capture of a workspace's output directory at
-- a point in time. `POST /v1/workspaces/from-snapshot` rebuilds a new
-- workspace from one.
--
-- Both tables are additive to 0001_init.sql. All statements are
-- idempotent so replays never break an existing database.

CREATE TABLE IF NOT EXISTS workspaces (
    id         text        PRIMARY KEY,
    created_at timestamptz NOT NULL DEFAULT now(),
    -- Soft-delete marker. When set, the workspace dirs are gone but
    -- snapshots taken from it keep the FK (set NULL via ON DELETE).
    deleted_at timestamptz,
    -- Filesystem paths, both under AGENTJAIL_STATE_DIR/workspaces/<id>/.
    source_dir text NOT NULL,
    output_dir text NOT NULL,
    -- Full JailConfig (minus runtime paths) as JSON. Re-applied on every
    -- exec so the user's network/seccomp/limits decisions persist.
    config     jsonb NOT NULL,
    -- Optional provenance for the initial clone.
    git_repo   text,
    git_ref    text,
    -- Human-readable tag shown in the dashboard.
    label      text
);
CREATE INDEX IF NOT EXISTS workspaces_created_at_idx ON workspaces (created_at DESC);
CREATE INDEX IF NOT EXISTS workspaces_deleted_at_idx ON workspaces (deleted_at);

CREATE TABLE IF NOT EXISTS snapshots (
    id           text PRIMARY KEY,
    -- Parent workspace. Set NULL when the workspace is hard-deleted, but
    -- the snapshot's content lives on independently.
    workspace_id text REFERENCES workspaces(id) ON DELETE SET NULL,
    name         text,
    created_at   timestamptz NOT NULL DEFAULT now(),
    -- On-disk path under AGENTJAIL_STATE_DIR/snapshots/<id>/.
    path         text  NOT NULL,
    size_bytes   bigint NOT NULL
);
CREATE INDEX IF NOT EXISTS snapshots_workspace_id_idx ON snapshots (workspace_id);
CREATE INDEX IF NOT EXISTS snapshots_created_at_idx   ON snapshots (created_at DESC);
