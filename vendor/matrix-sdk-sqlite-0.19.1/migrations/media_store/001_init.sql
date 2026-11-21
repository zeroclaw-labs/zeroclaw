-- basic kv metadata like the database version and store cipher
CREATE TABLE "kv" (
    "key" TEXT PRIMARY KEY NOT NULL,
    "value" BLOB NOT NULL
);

CREATE TABLE "media" (
    "uri" BLOB NOT NULL,
    "format" BLOB NOT NULL,
    "data" BLOB NOT NULL,
    "last_access" INTEGER NOT NULL,
    "ignore_policy" BOOLEAN NOT NULL DEFAULT FALSE,

    PRIMARY KEY ("uri", "format")
);

CREATE TABLE "lease_locks" (
    "key" TEXT PRIMARY KEY NOT NULL,
    "holder" TEXT NOT NULL,
    "expiration" REAL NOT NULL
);
