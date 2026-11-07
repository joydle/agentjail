-- Per-tenant upstream credentials.
--
-- Before this migration, `credentials` held one row per service — the
-- phantom proxy looked up the real key by service only, shared across
-- every tenant. Now it's one row per (tenant, service) pair so each
-- tenant brings its own OpenAI / Anthropic / GitHub / Stripe key.
--
-- Existing rows inherit the `"dev"` tenant — the sentinel the control
-- plane emits when auth is disabled + what `KeyStore::from_env()`
-- populates — so a single-tenant dev deployment keeps working with no
-- config change.

ALTER TABLE credentials
    ADD COLUMN IF NOT EXISTS tenant_id text NOT NULL DEFAULT 'dev';

-- Swap the primary key from `service` to `(tenant_id, service)`. Done
-- idempotently by dropping the old constraint if it still exists and
-- adding the new one only when it's absent.
ALTER TABLE credentials
    DROP CONSTRAINT IF EXISTS credentials_pkey;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'credentials_pkey'
    ) THEN
        ALTER TABLE credentials
            ADD CONSTRAINT credentials_pkey PRIMARY KEY (tenant_id, service);
    END IF;
END$$;
