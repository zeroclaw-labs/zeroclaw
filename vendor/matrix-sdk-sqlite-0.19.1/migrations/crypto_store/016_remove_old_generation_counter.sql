-- Remove generation counter tracking from the `kv` table, as this
-- is tracked in the `lease_locks` table.
DELETE FROM kv WHERE key = 'generation-counter';
