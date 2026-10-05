-- Add live-sample columns alongside the completion-time stats.
-- `memory_peak_bytes` stays (watermark); `memory_current_bytes` reflects
-- the latest sample; `pids_current` counts live processes.
ALTER TABLE jails ADD COLUMN IF NOT EXISTS memory_current_bytes BIGINT;
ALTER TABLE jails ADD COLUMN IF NOT EXISTS pids_current BIGINT;
