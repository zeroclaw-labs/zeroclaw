-- Add pairing_code column to tenants table.
-- Stores the one-time pairing code read from container logs so
-- the platform UI can display it to end users.
ALTER TABLE tenants ADD COLUMN pairing_code TEXT;
