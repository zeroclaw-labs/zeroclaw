-- We're changing the format of the linked chunk keys, and not migrating them over.
DELETE FROM "linked_chunks";

-- We're changing the name of `room_id` to `linked_chunk_id` in the linked chunks table, and it's
-- part of a primary key, so we'll recreate all the impacted tables.
DROP TABLE "event_chunks";
DROP TABLE "linked_chunks";
DROP TABLE "gap_chunks";

CREATE TABLE "linked_chunks" (
    -- Which linked chunk does this chunk belong to? (hashed key shared with the two other tables)
    "linked_chunk_id" BLOB NOT NULL,
    -- Identifier of the chunk, unique per room. Corresponds to a `ChunkIdentifier`.
    "id" INTEGER NOT NULL,

    -- Previous chunk in the linked list. Corresponds to a `ChunkIdentifier`.
    "previous" INTEGER,
    -- Next chunk in the linked list. Corresponds to a `ChunkIdentifier`.
    "next" INTEGER,
    -- Type of underlying entries: E for events, G for gaps
    "type" TEXT CHECK("type" IN ('E', 'G')) NOT NULL,

    -- Primary key is composed of the linked chunk ID and the chunk identifier.
    -- Such pairs must be unique.
    PRIMARY KEY (linked_chunk_id, id)
)
WITHOUT ROWID;

-- Entries inside an event chunk.
CREATE TABLE "event_chunks" (
    -- Which linked chunk does this event belong to? (hashed key shared with linked_chunks)
    "linked_chunk_id" BLOB NOT NULL,
    -- Which chunk does this event refer to? Corresponds to a `ChunkIdentifier`.
    "chunk_id" INTEGER NOT NULL,

    -- `OwnedEventId` for events.
    "event_id" BLOB NOT NULL,
    -- Position (index) in the chunk.
    "position" INTEGER NOT NULL,

    -- Primary key is the event ID.
    PRIMARY KEY (event_id),

    -- We need a uniqueness constraint over the `linked_chunk_id`, `chunk_id` and
    -- `position` tuple because (i) they must be unique, (ii) it dramatically
    -- improves the performance.
    UNIQUE (linked_chunk_id, chunk_id, position),

    -- If the owning chunk gets deleted, delete the entry too.
    FOREIGN KEY (linked_chunk_id, chunk_id) REFERENCES linked_chunks(linked_chunk_id, id) ON DELETE CASCADE
)
WITHOUT ROWID;

-- Gaps!
CREATE TABLE "gap_chunks" (
    -- Which linked chunk does this event belong to? (hashed key shared with linked_chunks)
    "linked_chunk_id" BLOB NOT NULL,
    -- Which chunk does this gap refer to? Corresponds to a `ChunkIdentifier`.
    "chunk_id" INTEGER NOT NULL,

    -- The previous batch token of a gap (encrypted value).
    "prev_token" BLOB NOT NULL,

    -- Primary key is composed of the linked chunk ID and the chunk identifier.
    -- Such pairs must be unique.
    PRIMARY KEY (linked_chunk_id, chunk_id),

    -- If the owning chunk gets deleted, delete the entry too.
    FOREIGN KEY (chunk_id, linked_chunk_id) REFERENCES linked_chunks(id, linked_chunk_id) ON DELETE CASCADE
)
WITHOUT ROWID;
