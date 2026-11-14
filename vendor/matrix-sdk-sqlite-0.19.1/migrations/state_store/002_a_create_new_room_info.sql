-- The rooms are not filtered by stripped state anymore but by state.
CREATE TABLE "new_room_info" (
    "room_id" BLOB PRIMARY KEY NOT NULL,
    "state" BLOB NOT NULL,
    "data" BLOB NOT NULL
);
