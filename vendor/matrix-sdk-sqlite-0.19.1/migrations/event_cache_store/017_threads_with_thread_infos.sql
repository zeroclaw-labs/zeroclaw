-- We need a place to store `ThreadInfo`, and this is in the `threads` table!

-- We delete the table because we don't want the new column to be optional, and
-- we cannot provide a default value (because it is encoded).
DROP TABLE "threads";

CREATE TABLE "threads" (
    -- This table offers a mapping between encoded `LinkedChunkId::Thread` and
    -- encoded `RoomId`.
    --
    -- Given a `RoomId`, it can be encoded, and then, thanks to this table,
    -- the encoded `LinkedChunkId` can be retrieved, which allows to query the
    -- other tables, like `linked_chunks`. In addition, the `EventId` can be
    -- retrieved, decoded, and used to built back the `LinkedChunkId`.

    -- The encoded `LinkedChunkId` (which is necessarily a `Thread` variant).
    --
    -- This value cannot be decoded.
    linked_chunk_id BLOB NOT NULL,

    -- The encoded `RoomId` from `LinkedChunkId::room_id()`.
    --
    -- This value cannot be decoded.
    room_id BLOB NOT NULL,

    -- The encoded `EventId` from `LinkedChunkId::Thread(_, event_id)`, so it is
    -- the thread root.
    --
    -- This value can be decoded.
    event_id BLOB NOT NULL,

    -- The encoded `ThreadInfo`.
    --
    -- This value can be decodedd.
    info BLOB NOT NULL,

    -- The unique values are `linked_chunk_id` and `event_id` (`room_id`
    -- cannot be unique — imagine two threads for the same room, the room ID is
    -- repeated twice). Having `event_id` in the primary key is not necessary.
    -- However, having `room_id` in the primary key is important to provide fast
    -- query from `room_id` to `linked_chunk_id`, which the primary purpose of
    -- this table (because a `PRIMARY KEY` will create an index automatically)!
    PRIMARY KEY (linked_chunk_id, room_id)
)
WITHOUT ROWID;
