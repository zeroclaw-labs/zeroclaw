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

    PRIMARY KEY ("uri", "format")
);
