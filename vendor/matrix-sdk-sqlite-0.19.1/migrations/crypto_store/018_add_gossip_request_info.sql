ALTER TABLE "key_requests"
    ADD COLUMN "info" BLOB;

CREATE UNIQUE INDEX "key_request_info_idx"
    ON "key_requests" ("info");
