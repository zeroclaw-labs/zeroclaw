-- Add an index to speed up queries that look for related events in a room.
CREATE INDEX "relates_to_idx"
    ON "events" ("room_id", "relates_to");

-- Add an index to speed up queries that look for related events in a room, with an additional
-- filter.
CREATE INDEX "relates_to_rel_type_idx"
    ON "events" ("room_id", "relates_to", "rel_type");
