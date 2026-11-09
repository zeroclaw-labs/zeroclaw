-- Outbound group sessions in some cases might want to persist a
-- Raw<AnyToDeviceEvent>, this does not work with MessagePack. Since it's fine
-- to rotate outbound group sessions let's force them to be rotated instead of
-- trying to salvage anything that was persisted.
DELETE FROM "outbound_group_session";
