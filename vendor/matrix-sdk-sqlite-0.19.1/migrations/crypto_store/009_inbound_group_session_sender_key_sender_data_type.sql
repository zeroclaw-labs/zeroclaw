ALTER TABLE "inbound_group_session"
    ADD COLUMN "sender_key" BLOB;

ALTER TABLE "inbound_group_session"
    ADD COLUMN "sender_data_type" INTEGER;

-- Create an index on sender curve25519 key and sender_data type, to help with
-- `get_inbound_group_sessions_for_device_batch`.
--
-- `session_id` is included so that the results are sorted by (hashed) session id.
CREATE INDEX "inbound_group_session_sender_key_sender_data_type_idx"
    ON "inbound_group_session" ("sender_key", "sender_data_type", "session_id");
