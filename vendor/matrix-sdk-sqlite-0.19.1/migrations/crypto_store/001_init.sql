CREATE TABLE "kv" (
    "key" TEXT PRIMARY KEY NOT NULL,
    "value" BLOB NOT NULL
);

CREATE TABLE "session" (
    "session_id" BLOB PRIMARY KEY NOT NULL,
    "sender_key" BLOB NOT NULL,
    "data" BLOB NOT NULL
);
CREATE INDEX "session_sender_key_idx"
    ON "session" ("sender_key");

CREATE TABLE "inbound_group_session" (
    "session_id" BLOB PRIMARY KEY NOT NULL,
    "room_id" BLOB NOT NULL,
    "backed_up" INTEGER NOT NULL,
    "data" BLOB NOT NULL
);
CREATE INDEX "inbound_group_session_room_id_idx"
    ON "inbound_group_session" ("room_id");

CREATE TABLE "outbound_group_session" (
    "room_id" BLOB PRIMARY KEY NOT NULL,
    "data" BLOB NOT NULL
);

CREATE TABLE "device" (
    "user_id" BLOB NOT NULL,
    "device_id" BLOB NOT NULL,
    "data" BLOB NOT NULL,

    PRIMARY KEY ("user_id", "device_id")
);
CREATE INDEX "device_user_id"
    ON "device" ("user_id");

CREATE TABLE "identity" (
    "user_id" BLOB PRIMARY KEY NOT NULL,
    "data" BLOB NOT NULL
);

CREATE TABLE "tracked_user" (
    "user_id" BLOB PRIMARY KEY NOT NULL,
    "data" BLOB NOT NULL
);

CREATE TABLE "olm_hash" (
    "data" BLOB PRIMARY KEY NOT NULL
);

CREATE TABLE "key_requests" (
    "request_id" BLOB PRIMARY KEY NOT NULL,
    "sent_out" INTEGER NOT NULL,
    "data" BLOB NOT NULL
);
