-- Delete all previous entries in the dependent send queue table, because the
-- format of `parent_key` has changed.
DELETE FROM "dependent_send_queue_events";

--- Delete all previous entries in the send queue too.
DELETE FROM "send_queue_events";
