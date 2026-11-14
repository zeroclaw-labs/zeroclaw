-- After the merge of https://github.com/matrix-org/matrix-rust-sdk/pull/5648,
-- we want all events to get a `TimelineEvent::timestamp` value (extracted from
-- `origin_server_ts`).
--
-- To accomplish that, we are emptying the event cache. New synced events will
-- be built correctly, with a valid `TimelineEvent::timestamp`, allowing a
-- clear, stable situation.

DELETE from linked_chunks;
DELETE from event_chunks; -- should be done by cascading
DELETE from gap_chunks; -- should be done by cascading
DELETE from events;
