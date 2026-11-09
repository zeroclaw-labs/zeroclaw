-- Remove uniqueness constraint on event_id, replace with regular index.
-- Empties event cache as data migration is not needed.

DELETE FROM linked_chunks;
DELETE FROM event_chunks;  -- should be done by cascading
DELETE FROM gap_chunks;    -- should be done by cascading

DROP TABLE event_chunks;

-- Recreate event_chunks with new PRIMARY KEY
CREATE TABLE "event_chunks" (
    -- Which linked chunk does this event belong to? (hashed key shared with linked_chunks)
    "linked_chunk_id" BLOB NOT NULL,
    -- Which chunk does this event refer to? Corresponds to a `ChunkIdentifier`.
    "chunk_id" INTEGER NOT NULL,

    -- `OwnedEventId` for events.
    "event_id" BLOB NOT NULL,
    -- Position (index) in the chunk.
    "position" INTEGER NOT NULL,

    -- We need a uniqueness constraint over the `linked_chunk_id`, `chunk_id` and
    -- `position` tuple because (i) they must be unique, (ii) it dramatically
    -- improves the performance. Also, we don't have a ROWID, so we must use a PRIMARY KEY, hence
    -- we use this composite key as the primary key.
    PRIMARY KEY (linked_chunk_id, chunk_id, position),

    -- If the owning chunk gets deleted, delete the entry too.
    FOREIGN KEY (linked_chunk_id, chunk_id) REFERENCES linked_chunks(linked_chunk_id, id) ON DELETE CASCADE
)
WITHOUT ROWID;

-- Create a non-unique index on `event_id` for query performance.
CREATE INDEX "event_chunks_event_id_idx" ON "event_chunks" ("event_id");
