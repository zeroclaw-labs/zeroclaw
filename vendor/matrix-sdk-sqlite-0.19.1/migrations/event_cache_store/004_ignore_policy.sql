-- Add an ignore_policy column, defaulting to FALSE for all media content.
ALTER TABLE "media"
    ADD COLUMN "ignore_policy" BOOLEAN NOT NULL DEFAULT FALSE;
