-- Rename the `expiration_ts` column to `expiration` to be consistent with other
-- `lease_locks` tables in other stores.
ALTER TABLE "lease_locks" RENAME COLUMN "expiration_ts" TO "expiration";

-- Add the `generation` column to handle _dirtiness.
-- Default value is `FIRST_CROSS_PROCESS_LOCK_GENERATION`.
ALTER TABLE "lease_locks" ADD COLUMN "generation" INTEGER NOT NULL DEFAULT 1;
