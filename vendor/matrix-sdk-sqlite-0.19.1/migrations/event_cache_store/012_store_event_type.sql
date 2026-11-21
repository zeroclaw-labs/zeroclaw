-- For the event decryption to happen in the event cache we need the ability to
-- fetch only `m.room.encrypted` events out of the store.
--
-- To accomplish that, we are emptying the event cache. New events inserted with
-- the event type and the session ID of the room key as separate columns.

DELETE from linked_chunks;
DELETE from event_chunks; -- should be done by cascading
DELETE from gap_chunks; -- should be done by cascading
DELETE from events;

DROP TABLE events;

-- Events and their content.
CREATE TABLE "events" (
    -- The room in which the event is located.
    "room_id" BLOB NOT NULL,

    -- The `OwnedEventId` of this event.
    "event_id" BLOB NOT NULL,

    -- The event type of this event.
    "event_type" BLOB NOT NULL,

    -- The ID of the session that was used to encrypt this event, may be null if
    -- the event wasn't encrypted.
    "session_id" BLOB NULL,

    -- JSON serialized `TimelineEvent` (encrypted value).
    "content" BLOB NOT NULL,

    -- If this event is an aggregation (related event), the event id of the event it relates to.
    -- Can be null if this event isn't an aggregation.
    "relates_to" BLOB,

    -- If this event is an aggregation (related event), the kind of relation it has to the event it
    -- relates to.
    -- Can be null if this event isn't an aggregation.
    "rel_type" BLOB,

    -- Primary key is the event ID.
    PRIMARY KEY (event_id)
)
WITHOUT ROWID;

-- Add an index to speed up queries that look for related events in a room.
CREATE INDEX "relates_to_idx"
    ON "events" ("room_id", "relates_to");

-- Add an index to speed up queries that look for related events in a room, with an additional
-- filter.
CREATE INDEX "relates_to_rel_type_idx"
    ON "events" ("room_id", "relates_to", "rel_type");

-- Add an index to speed up queries that look for related events in a room.
CREATE INDEX "event_type_index"
    ON "events" ("room_id", "event_type", "session_id");
