-- We're going to split the `events` table into two tables: `events` and `event_chunks`.
-- The former table will include the events' content, while the latter will include the location of
-- each event in the linked chunk.

-- Since we're going to get rid of the event chunks, we have to empty all the linked chunks.
DELETE FROM "linked_chunks";

-- Delete the events table that contains entries into an event chunk, along with the content of
-- those events.
DROP TABLE "events";

-- Events and their content.
CREATE TABLE "events" (
    -- The room in which the event is located.
    "room_id" BLOB NOT NULL,

    -- The `OwnedEventId` of this event.
    "event_id" BLOB NOT NULL,

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

-- Entries inside an event chunk.
CREATE TABLE "event_chunks" (
    -- Which room does this event belong to? (hashed key shared with linked_chunks)
    "room_id" BLOB NOT NULL,
    -- Which chunk does this event refer to? Corresponds to a `ChunkIdentifier`.
    "chunk_id" INTEGER NOT NULL,

    -- `OwnedEventId` for events.
    "event_id" BLOB NOT NULL,
    -- Position (index) in the chunk.
    "position" INTEGER NOT NULL,

    -- Primary key is the event ID.
    PRIMARY KEY (event_id),

    -- We need a uniqueness constraint over the `room_id`, `chunk_id` and
    -- `position` tuple because (i) they must be unique, (ii) it dramatically
    -- improves the performance.
    UNIQUE (room_id, chunk_id, position),

    -- If the owning chunk gets deleted, delete the entry too.
    FOREIGN KEY (room_id, chunk_id) REFERENCES linked_chunks(room_id, id) ON DELETE CASCADE
)
WITHOUT ROWID;

-- For consistency, rename gaps to gap_chunks.
ALTER TABLE gaps RENAME TO gap_chunks;
