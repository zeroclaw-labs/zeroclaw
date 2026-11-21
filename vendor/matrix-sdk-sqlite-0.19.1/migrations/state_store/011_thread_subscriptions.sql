-- Thread subscriptions.
CREATE TABLE "thread_subscriptions" (
    "room_id" BLOB NOT NULL,
    -- Event ID of the thread root.
    "event_id" BLOB NOT NULL,
    "status" TEXT NOT NULL,

    PRIMARY KEY ("room_id", "event_id")
);
