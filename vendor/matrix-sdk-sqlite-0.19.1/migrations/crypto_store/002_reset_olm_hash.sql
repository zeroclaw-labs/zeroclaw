-- Hashes in the olm_hash table were initially stored as JSON, even though
-- everything else is MessagePack. Alongside this migration, the encoding is
-- updated.
DELETE FROM "olm_hash";
