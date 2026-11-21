-- Delete all previous entries in the dependent send queue table, because the format changed.
DELETE FROM "dependent_send_queue_events";

-- Rename its "event_id" column to "parent_key", while we're at it.
ALTER TABLE "dependent_send_queue_events"
    RENAME COLUMN "event_id"
    TO "parent_key";

--- Delete all previous entries in the send queue, since the content's format has changed.
DELETE FROM "send_queue_events";
