-- Outbound group sessions changed their format, now we remember which members
-- received a withheld code. To not throw errors when trying to restore such
-- sessions just drop them so they get rotated.
DELETE FROM "outbound_group_session";
