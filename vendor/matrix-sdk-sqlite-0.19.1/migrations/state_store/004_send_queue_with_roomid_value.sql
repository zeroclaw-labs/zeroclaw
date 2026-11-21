-- Send queue events, keyed by room id and transaction id, but also include the room id in the
-- value.
DROP TABLE "send_queue_events";

CREATE TABLE "send_queue_events" (
    -- This is used as a key, thus hashed.
    "room_id" BLOB NOT NULL,

    -- This is used as a value (thus encrypted/decrypted).
    "room_id_val" BLOB NOT NULL,

    -- This is used as both a key and a value, thus neither encrypted/decrypted/hashed.
    "transaction_id" BLOB NOT NULL,

    -- Used as a value, thus encrypted/decrypted.
    "content" BLOB NOT NULL,

    -- In clear.
    "wedged" BOOLEAN NOT NULL,

    PRIMARY KEY ("room_id", "transaction_id")
);
