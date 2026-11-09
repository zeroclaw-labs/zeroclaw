-- Add a column bump_stamp (number) to the table thread_subscriptions.
ALTER TABLE "thread_subscriptions"
    ADD COLUMN "bump_stamp" INTEGER;
