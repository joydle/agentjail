-- Per-workspace inbound domain mapping.
--
-- Each row in the `domains` jsonb array has shape:
--   { "domain": "ws-abc.dev.agentjail", "backend_url": "http://10.0.0.5:3000" }
--
-- The agentjail-server gateway listener reads this map and forwards any
-- incoming HTTP request whose `Host` header matches to the backend URL.
-- `backend_url` is supplied by the caller (the gateway does not discover
-- jail-internal IPs). Pair with veth-NAT, a side-car tunnel, or plain
-- Docker networking depending on your deployment.

ALTER TABLE workspaces
    ADD COLUMN IF NOT EXISTS domains jsonb NOT NULL DEFAULT '[]'::jsonb;

-- Matching key for the gateway's hostname lookup. We keep the index
-- loose — per-row JSON scan is fine at the scale the gateway cares
-- about, but the GIN index keeps it snappy past a few hundred rows.
CREATE INDEX IF NOT EXISTS workspaces_domains_gin_idx ON workspaces USING GIN (domains);
