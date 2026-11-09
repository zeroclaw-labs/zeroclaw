DROP INDEX "linked_chunks_id_and_room_id";
DROP INDEX "linked_chunks_event_id_and_room_id";
DROP TABLE "events";
DROP TABLE "gaps";
DROP TABLE "linked_chunks";

CREATE TABLE "linked_chunks" (
    -- Which room does this chunk belong to? (hashed key shared with the two other tables)
    "room_id" BLOB NOT NULL,
    -- Identifier of the chunk, unique per room. Corresponds to a `ChunkIdentifier`.
    "id" INTEGER NOT NULL,

    -- Previous chunk in the linked list. Corresponds to a `ChunkIdentifier`.
    "previous" INTEGER,
    -- Next chunk in the linked list. Corresponds to a `ChunkIdentifier`.
    "next" INTEGER,
    -- Type of underlying entries: E for events, G for gaps
    "type" TEXT CHECK("type" IN ('E', 'G')) NOT NULL,

    -- Primary key is composed of the room ID and the chunk identifier.
    -- Such pairs must be unique.
    PRIMARY KEY (room_id, id)
)
WITHOUT ROWID;

CREATE TABLE "gaps" (
    -- Which room does this event belong to? (hashed key shared with linked_chunks)
    "room_id" BLOB NOT NULL,
    -- Which chunk does this gap refer to? Corresponds to a `ChunkIdentifier`.
    "chunk_id" INTEGER NOT NULL,

    -- The previous batch token of a gap (encrypted value).
    "prev_token" BLOB NOT NULL,

    -- Primary key is composed of the room ID and the chunk identifier.
    -- Such pairs must be unique.
    PRIMARY KEY (room_id, chunk_id),

    -- If the owning chunk gets deleted, delete the entry too.
    FOREIGN KEY (chunk_id, room_id) REFERENCES linked_chunks(id, room_id) ON DELETE CASCADE
)
WITHOUT ROWID;

-- Items for an event chunk.
CREATE TABLE "events" (
    -- Which room does this event belong to? (hashed key shared with linked_chunks)
    "room_id" BLOB NOT NULL,
    -- Which chunk does this event refer to? Corresponds to a `ChunkIdentifier`.
    "chunk_id" INTEGER NOT NULL,

    -- `OwnedEventId` for events.
    "event_id" BLOB NOT NULL,
    -- JSON serialized `TimelineEvent` (encrypted value).
    "content" BLOB NOT NULL,
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
