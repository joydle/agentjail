-- Store phantom tokens as SHA-256 digests.
--
-- 0009 named the key column `token_hash` but the store wrote the raw
-- 32 token bytes into it, so a database read (backup, replica, SQL
-- injection elsewhere) yielded directly replayable phantom tokens.
-- From now on the column holds sha256(raw_token_bytes).
--
-- Migrations replay on every boot, so the rehash is guarded by a marker
-- column: existing rows get `token_hashed = false` once, are hashed in
-- place and flipped to true; the column default then becomes true so
-- rows written by the new code are never hashed twice.
-- Requires PostgreSQL >= 11 (built-in sha256()).

ALTER TABLE phantom_tokens
    ADD COLUMN IF NOT EXISTS token_hashed boolean NOT NULL DEFAULT false;

UPDATE phantom_tokens
SET    token_hash = sha256(token_hash),
       token_hashed = true
WHERE  token_hashed = false;

ALTER TABLE phantom_tokens
    ALTER COLUMN token_hashed SET DEFAULT true;
