-- After the data was migrated, we need to replace the old table with the new.
DROP TABLE "room_info";
ALTER TABLE "new_room_info" RENAME TO "room_info";

CREATE INDEX "room_info_room_id_state"
    ON "room_info" ("room_id", "state");
