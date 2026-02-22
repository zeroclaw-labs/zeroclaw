CREATE TABLE IF NOT EXISTS resource_snapshots (
    id          TEXT PRIMARY KEY,
    tenant_id   TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    ts          TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    cpu_pct     REAL NOT NULL DEFAULT 0,
    mem_bytes   INTEGER NOT NULL DEFAULT 0,
    mem_limit   INTEGER NOT NULL DEFAULT 0,
    disk_bytes  INTEGER NOT NULL DEFAULT 0,
    net_in_bytes  INTEGER NOT NULL DEFAULT 0,
    net_out_bytes INTEGER NOT NULL DEFAULT 0,
    pids        INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_resource_tenant_ts ON resource_snapshots(tenant_id, ts);
