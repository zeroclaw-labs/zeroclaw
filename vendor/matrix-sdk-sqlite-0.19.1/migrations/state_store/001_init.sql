-- basic kv data like the database version and store cipher
CREATE TABLE "kv" (
    "key" TEXT PRIMARY KEY NOT NULL,
    "value" BLOB NOT NULL
);

-- another general-purpose kv table, but with blob keys
-- used for sync token, filters and other things
--
-- in theory, we could use one table for both since sqlite has dynamic typing
-- (column types are basically meaningless), but this is a cleaner solution
CREATE TABLE "kv_blob" (
    "key" BLOB PRIMARY KEY NOT NULL,
    "value" BLOB NOT NULL
);

CREATE TABLE "room_info" (
    "room_id" BLOB PRIMARY KEY NOT NULL,
    "stripped" BOOLEAN NOT NULL,
    "data" BLOB NOT NULL
);
CREATE INDEX "room_info_room_id_stripped"
    ON "room_info" ("room_id", "stripped");

CREATE TABLE "state_event" (
    "room_id" BLOB NOT NULL,
    "event_type" BLOB NOT NULL,
    "state_key" BLOB NOT NULL,
    "stripped" BOOLEAN NOT NULL,
    "event_id" BLOB,
    "data" BLOB NOT NULL,

    PRIMARY KEY ("room_id", "event_type", "state_key")
);
CREATE INDEX "state_event_room_id_event_type_stripped_event_id"
    ON "state_event" ("room_id", "event_type", "stripped", "event_id");

CREATE TABLE "global_account_data" (
    "event_type" BLOB PRIMARY KEY NOT NULL,
    "data" BLOB NOT NULL
);

CREATE TABLE "room_account_data" (
    "room_id" BLOB NOT NULL,
    "event_type" BLOB NOT NULL,
    "data" BLOB NOT NULL,

    PRIMARY KEY ("room_id", "event_type")
);

CREATE TABLE "member" (
    "room_id" BLOB NOT NULL,
    "user_id" BLOB NOT NULL,
    "membership" BLOB NOT NULL,
    "stripped" BOOLEAN NOT NULL,
    "data" BLOB NOT NULL,

    PRIMARY KEY ("room_id", "user_id")
);
CREATE INDEX "member_room_id_membership"
    ON "member" ("room_id", "membership");

CREATE TABLE "profile" (
    "room_id" BLOB NOT NULL,
    "user_id" BLOB NOT NULL,
    "data" BLOB NOT NULL,

    PRIMARY KEY ("room_id", "user_id")
);

CREATE TABLE "receipt" (
    "room_id" BLOB NOT NULL,
    "user_id" BLOB NOT NULL,
    "receipt_type" BLOB NOT NULL,
    "thread" BLOB NOT NULL,
    "event_id" BLOB NOT NULL,
    "data" BLOB NOT NULL,

    PRIMARY KEY ("room_id", "user_id", "receipt_type", "thread")
);
CREATE INDEX "receipt_room_id_receipt_type_thread_id_event_id"
    ON "receipt" ("room_id", "receipt_type", "thread", "event_id");

CREATE TABLE "display_name" (
    "room_id" BLOB NOT NULL,
    "name" BLOB NOT NULL,
    "data" BLOB NOT NULL,

    PRIMARY KEY ("room_id", "name")
);

CREATE TABLE "media" (
    "uri" BLOB NOT NULL,
    "format" BLOB NOT NULL,
    "data" BLOB NOT NULL,

    PRIMARY KEY ("uri", "format")
);
