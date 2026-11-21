CREATE TABLE "direct_withheld_info" (
    "session_id" BLOB PRIMARY KEY NOT NULL,
    "room_id" BLOB NOT NULL,
    "data" BLOB NOT NULL
);
