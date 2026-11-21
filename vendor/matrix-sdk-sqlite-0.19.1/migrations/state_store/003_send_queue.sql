-- Send queue events, keyed by room id and transaction id.
CREATE TABLE "send_queue_events" (
    "room_id" BLOB NOT NULL,
    "transaction_id" BLOB NOT NULL,
    "content" BLOB NOT NULL,
    "wedged" BOOLEAN NOT NULL,

    PRIMARY KEY ("room_id", "transaction_id")
);
