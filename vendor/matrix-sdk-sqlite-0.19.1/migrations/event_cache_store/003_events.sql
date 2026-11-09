CREATE TABLE "linked_chunks" (
    -- Identifier of the chunk, unique per room. Corresponds to a `ChunkIdentifier`.
    "id" INTEGER,
    -- Which room does this chunk belong to? (hashed key shared with the two other tables)
    "room_id" BLOB NOT NULL,

    -- Previous chunk in the linked list. Corresponds to a `ChunkIdentifier`.
    "previous" INTEGER,
    -- Next chunk in the linked list. Corresponds to a `ChunkIdentifier`.
    "next" INTEGER,
    -- Type of underlying entries: E for events, G for gaps
    "type" TEXT CHECK("type" IN ('E', 'G')) NOT NULL
);

CREATE UNIQUE INDEX "linked_chunks_id_and_room_id" ON linked_chunks (id, room_id);

CREATE TABLE "gaps" (
    -- Which chunk does this gap refer to? Corresponds to a `ChunkIdentifier`.
    "chunk_id" INTEGER NOT NULL,
    -- Which room does this event belong to? (hashed key shared with linked_chunks)
    "room_id" BLOB NOT NULL,

    -- The previous batch token of a gap (encrypted value).
    "prev_token" BLOB NOT NULL,

    -- If the owning chunk gets deleted, delete the entry too.
    FOREIGN KEY(chunk_id, room_id) REFERENCES linked_chunks(id, room_id) ON DELETE CASCADE
);

-- Items for an event chunk.
CREATE TABLE "events" (
    -- Which chunk does this event refer to? Corresponds to a `ChunkIdentifier`.
    "chunk_id" INTEGER NOT NULL,
    -- Which room does this event belong to? (hashed key shared with linked_chunks)
    "room_id" BLOB NOT NULL,

    -- `OwnedEventId` for events, can be null if malformed.
    "event_id" TEXT,
    -- JSON serialized `TimelineEvent` (encrypted value).
    "content" BLOB NOT NULL,
    -- Position (index) in the chunk.
    "position" INTEGER NOT NULL,

    -- If the owning chunk gets deleted, delete the entry too.
    FOREIGN KEY(chunk_id, room_id) REFERENCES linked_chunks(id, room_id) ON DELETE CASCADE
);
