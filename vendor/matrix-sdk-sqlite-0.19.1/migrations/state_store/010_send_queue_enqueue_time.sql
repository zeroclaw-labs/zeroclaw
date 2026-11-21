-- Migration script to add the created_at column to the send_queue_events table
ALTER TABLE "send_queue_events"
ADD COLUMN "created_at" INTEGER DEFAULT NULL;

ALTER TABLE "dependent_send_queue_events"
ADD COLUMN "created_at" INTEGER DEFAULT NULL;
