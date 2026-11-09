-- Send queue dependent events
CREATE TABLE "dependent_send_queue_events" (
    -- This is used as a key, thus hashed.
    "room_id" BLOB NOT NULL,

    -- This is used as both a key and a value, thus neither encrypted/decrypted/hashed.
    -- This is the transaction id for the *parent* transaction, not our own.
    "parent_transaction_id" BLOB NOT NULL,

    -- This is used as both a key and a value, thus neither encrypted/decrypted/hashed.
    -- This is a transaction id used for the dependent event itself, not the parent.
    "own_transaction_id" BLOB NOT NULL,

    -- Used as a value (thus encrypted/decrypted), can be null.
    "event_id" BLOB NULL,

    -- Serialized `DependentQueuedRequestKind`, used as a value (thus encrypted/decrypted).
    "content" BLOB NOT NULL
);
