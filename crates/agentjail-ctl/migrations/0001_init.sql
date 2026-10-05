-- Initial schema. Embedded via include_str! and applied idempotently at
-- startup. Keep every statement additive (CREATE IF NOT EXISTS, ADD COLUMN)
-- so replays never break on existing databases.

CREATE TABLE IF NOT EXISTS credentials (
    service     text        PRIMARY KEY,
    secret      text        NOT NULL,
    fingerprint text        NOT NULL,
    added_at    timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS audit_log (
    id            bigserial PRIMARY KEY,
    at            timestamptz NOT NULL DEFAULT now(),
    session_id    text        NOT NULL,
    service       text        NOT NULL,
    method        text        NOT NULL,
    path          text        NOT NULL,
    status        integer     NOT NULL,
    reject_reason text,
    upstream_ms   bigint
);
CREATE INDEX IF NOT EXISTS audit_log_at_idx ON audit_log (at DESC);

CREATE TABLE IF NOT EXISTS jails (
    id                bigserial   PRIMARY KEY,
    kind              text        NOT NULL,
    started_at        timestamptz NOT NULL DEFAULT now(),
    ended_at          timestamptz,
    status            text        NOT NULL,
    session_id        text,
    label             text        NOT NULL,
    exit_code         integer,
    duration_ms       bigint,
    timed_out         boolean,
    oom_killed        boolean,
    memory_peak_bytes bigint,
    cpu_usage_usec    bigint,
    io_read_bytes     bigint,
    io_write_bytes    bigint,
    stdout            text,
    stderr            text,
    error             text
);
CREATE INDEX IF NOT EXISTS jails_started_at_idx ON jails (started_at DESC);
CREATE INDEX IF NOT EXISTS jails_status_idx     ON jails (status);

-- Fork-graph link: children point at their parent row. NULL for non-fork
-- or root fork rows. Additive to support replays.
ALTER TABLE jails ADD COLUMN IF NOT EXISTS parent_id bigint
    REFERENCES jails(id) ON DELETE SET NULL;
CREATE INDEX IF NOT EXISTS jails_parent_id_idx ON jails (parent_id);
