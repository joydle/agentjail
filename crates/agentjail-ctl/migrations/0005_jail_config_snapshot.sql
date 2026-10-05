-- Jail config snapshot — "what did this run with?"
--
-- Stored as JSONB so the schema doesn't have to track every knob;
-- the rust-side `JailConfigSnapshot` is the source of truth for the
-- shape. Null for legacy rows predating this column.

ALTER TABLE jails
    ADD COLUMN IF NOT EXISTS config_json JSONB;
