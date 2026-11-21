CREATE TABLE "lease_locks" (
    "key" TEXT PRIMARY KEY NOT NULL,
    "holder" TEXT NOT NULL,
    "expiration_ts" REAL NOT NULL
);
