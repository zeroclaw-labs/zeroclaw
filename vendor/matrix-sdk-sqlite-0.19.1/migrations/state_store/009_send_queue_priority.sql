-- Add a priority column, defaulting to 0 for all events in the send queue.
ALTER TABLE "send_queue_events"
    ADD COLUMN "priority" INTEGER NOT NULL DEFAULT 0;
