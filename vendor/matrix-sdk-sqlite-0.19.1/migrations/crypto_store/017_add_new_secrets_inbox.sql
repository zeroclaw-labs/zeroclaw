CREATE TABLE "secrets_inbox" (
    "secret_name" BLOB NOT NULL,
    "secret" BLOB NOT NULL
);
CREATE UNIQUE INDEX "secrets_inbox_secret_name_secret_idx"
    ON "secrets_inbox" ("secret_name", "secret");
