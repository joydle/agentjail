-- Durable sessions + phantom tokens.
--
-- Before this migration both lived in process memory: a control-plane
-- restart kicked every in-flight agent, and a second instance couldn't
-- see sessions the first had minted. Now the DB is the source of
-- truth; an in-memory LRU still fronts the hot-path token lookup
-- (phantom proxy reads the mapping on every upstream call).
--
-- Background reaper (`sessions_tokens_reaper`) sweeps expired rows so
-- the tables don't balloon. TTL semantics: `expires_at` is NULL when
-- the caller didn't set a TTL; those rows live until explicit revoke
-- or cascade on session delete.

CREATE TABLE IF NOT EXISTS sessions (
    id          text        PRIMARY KEY,            -- sess_<24hex>
    tenant_id   text        NOT NULL DEFAULT 'dev',
    created_at  timestamptz NOT NULL DEFAULT now(),
    expires_at  timestamptz,                        -- NULL = no expiry
    services    jsonb       NOT NULL,               -- Vec<ServiceId>
    env         jsonb       NOT NULL                -- HashMap<String, String>
);
CREATE INDEX IF NOT EXISTS sessions_tenant_created_idx
    ON sessions (tenant_id, created_at DESC);
CREATE INDEX IF NOT EXISTS sessions_expires_at_idx
    ON sessions (expires_at) WHERE expires_at IS NOT NULL;

CREATE TABLE IF NOT EXISTS phantom_tokens (
    -- BLAKE3/SHA256 of the raw token bytes. We never store the raw
    -- token — it's uniform-random, so the hash is a lossless key and
    -- a DB leak can't be replayed against the proxy.
    token_hash  bytea       PRIMARY KEY,
    session_id  text        NOT NULL
                REFERENCES sessions(id) ON DELETE CASCADE,
    tenant_id   text        NOT NULL DEFAULT 'dev',
    service     text        NOT NULL,               -- "openai" / "anthropic" / …
    scope       jsonb       NOT NULL,               -- Scope
    created_at  timestamptz NOT NULL DEFAULT now(),
    expires_at  timestamptz
);
CREATE INDEX IF NOT EXISTS phantom_tokens_session_idx
    ON phantom_tokens (session_id);
CREATE INDEX IF NOT EXISTS phantom_tokens_expires_idx
    ON phantom_tokens (expires_at) WHERE expires_at IS NOT NULL;
