-- Gateway hostnames are globally unique.
--
-- Before this migration `workspaces.domains` was a jsonb array with only
-- a GIN index, and the gateway's lookup was `... domains @> $1 LIMIT 1`.
-- Any tenant could declare a hostname another tenant already used and,
-- depending on row order, take over its traffic.
--
-- `workspace_domains` holds one row per (live) declared hostname with
-- the hostname as PRIMARY KEY, so a second claim fails with
-- unique_violation (surfaced as 409 Conflict). Rows are removed when the
-- owning workspace is soft-deleted.
--
-- Idempotent: replays are no-ops (IF NOT EXISTS / ON CONFLICT DO NOTHING).

CREATE TABLE IF NOT EXISTS workspace_domains (
    domain        text PRIMARY KEY,
    workspace_id  text NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    created_at    timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS workspace_domains_workspace_idx
    ON workspace_domains (workspace_id);

-- Backfill from live rows. On pre-existing duplicates the oldest
-- workspace keeps the hostname; later claimants lose routing (they were
-- the hijack candidates).
INSERT INTO workspace_domains (domain, workspace_id)
SELECT lower(d->>'domain'), w.id
FROM   workspaces w
CROSS  JOIN LATERAL jsonb_array_elements(w.domains) AS d
WHERE  w.deleted_at IS NULL
  AND  coalesce(d->>'domain', '') <> ''
ORDER  BY w.created_at ASC, w.id ASC
ON CONFLICT (domain) DO NOTHING;
