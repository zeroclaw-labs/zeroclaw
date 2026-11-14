-- The following columns now contain encoded data:
-- 
-- - `event_chunks.event_id`,
-- - `events.event_id`,
-- - `events.relates_to`.
-- 
-- Nothing to do except clearing all the data.
-- 
-- Event IDs are encoded as _keys_: They cannot be decoded. However, keep in
-- mind that the event's content is encoded as a _value_ and can be decoded: it
-- contains the event ID. However, it is not possible to query it.

DELETE FROM linked_chunks;
DELETE FROM event_chunks;  -- should be done by cascading
DELETE FROM gap_chunks;    -- should be done by cascading
DELETE FROM events;
