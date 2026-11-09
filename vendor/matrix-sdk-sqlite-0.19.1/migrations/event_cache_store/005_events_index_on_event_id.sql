-- Create a unique index on `events.event_id` and `events.room_id` .
CREATE UNIQUE INDEX "linked_chunks_event_id_and_room_id" ON events (event_id, room_id);
