-- Add the `generation` column to handle _dirtiness_.
-- Default value is `FIRST_CROSS_PROCESS_LOCK_GENERATION`.
ALTER TABLE "lease_locks" ADD COLUMN "generation" INTEGER NOT NULL DEFAULT 1;
