-- New send queue events, now persists the type of error causing it to be wedged
ALTER TABLE "send_queue_events"
    -- The serialized json (bytes) representing the error. Used as a value, thus encrypted/decrypted.
    -- NULLABLE field (default NULL)
    ADD COLUMN "wedge_reason" BLOB;
