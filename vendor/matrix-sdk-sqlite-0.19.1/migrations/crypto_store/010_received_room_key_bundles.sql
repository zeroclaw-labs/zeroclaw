CREATE TABLE "received_room_key_bundle"
(
    "room_id"        BLOB NOT NULL,
    "sender_user_id" BLOB NOT NULL,
    "bundle_data"    BLOB NOT NULL
);

CREATE UNIQUE INDEX "received_room_key_bundle_room_id_user_id_idx"
    ON "received_room_key_bundle" ("room_id", "sender_user_id");
