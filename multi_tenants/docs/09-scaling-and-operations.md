# 09 — Scaling and Operations

## Capacity Planning

### Per-Tenant Resource Overhead

| Resource | Baseline per Tenant |
|----------|---------------------|
| Memory   | ~7 MB base (idle container) + ephemeral spikes during LLM calls |
| CPU      | Mostly idle between calls; LLM inference is offloaded to external API |
| Disk     | ~50 MB per tenant (agent binary, config, local memory store) |
| Network  | Proportional to message volume; typically low sustained throughput |

### Single-Node Limits (Rough Estimates)

| Host RAM | Tenant Capacity |
|----------|----------------|
| 4 GB     | ~200 tenants   |
| 8 GB     | ~500 tenants   |
| 16 GB    | ~1,000 tenants |

Limits assume moderate message volume. CPU-bound workloads (heavy tool use, frequent calls) reduce headroom. Monitor
`resource_snapshots.cpu_pct` trends over 24 h before adding tenants near the ceiling.

### Disk Budget

- Platform DB (`zcplatform.db`): grows with audit log and snapshot retention; expect 100–500 MB for 500 tenants at 7-day retention.
- Container images: ~200 MB for the ZeroClaw agent image (shared, not per-tenant).
- Backups: one archive per backup run; size ≈ DB size + vault key file (<1 MB).

---

## Monitoring Stack

### Collectors

| Collector         | Interval | Table              | Retention |
|-------------------|----------|--------------------|-----------|
| resource_collector | 60 s    | `resource_snapshots` | 7 days  |
| usage_collector   | 5 min    | `usage_metrics`    | configurable |
| health_checker    | 30 s     | in-memory + DB status | live  |

**resource_collector** runs `docker stats --no-stream --format json` and `du -sb` per tenant, storing: `cpu_pct`, `mem_bytes`, `mem_limit`, `disk_bytes`, `net_in`, `net_out`, `pids`.

**usage_collector** scrapes each tenant's `/metrics` endpoint (Prometheus format) for `zeroclaw_messages_total` and `zeroclaw_tokens_total{direction=input|output}`, then aggregates hourly into `usage_metrics`.

**health_checker** polls running tenants every 30 s. On failure it attempts one auto-restart; if restart fails, the tenant is marked `error` status and logged to the audit trail.

### Admin Dashboard

- `GET /api/monitoring/dashboard` — fleet summary (total/running/stopped/error tenants, users, channels).
- `GET /api/monitoring/health` — per-tenant health status with uptime.
- `GET /api/monitoring/resources` — current resource summary across all tenants.
- `GET /api/tenants/{id}/resources?range=1h|6h|24h|7d` — historical resource chart data for one tenant.

---

## Operational Runbook

### Restart a Tenant

```bash
# Via API (Manager role or above)
curl -X POST https://platform.example.com/api/tenants/{id}/restart \
  -H "Authorization: Bearer $TOKEN"

# Via admin dashboard: Tenants -> tenant row -> Restart
```

### View Tenant Logs

```bash
# Last 100 lines via API
curl "https://platform.example.com/api/tenants/{id}/logs?tail=100" \
  -H "Authorization: Bearer $TOKEN"

# Direct Docker access on the host
docker logs zeroclaw-tenant-{id} --tail 100 -f
```

### Check Fleet Health

```bash
curl https://platform.example.com/api/monitoring/health \
  -H "Authorization: Bearer $TOKEN" | jq '.[] | select(.status == "error")'
```

### Force-Stop a Stuck Container

```bash
# Via API
curl -X POST https://platform.example.com/api/tenants/{id}/stop \
  -H "Authorization: Bearer $TOKEN"

# Direct Docker fallback if API is unresponsive
docker stop zeroclaw-tenant-{id}
docker rm zeroclaw-tenant-{id}
```

### Run a Diagnostic Command in a Container

```bash
curl -X POST https://platform.example.com/api/tenants/{id}/exec \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"command": "zeroclaw status"}'
```

---

## Backup and Restore

### What Gets Backed Up

- `zcplatform.db` — full SQLite database (tenants, users, config, audit log, metrics).
- Vault key file — master encryption key for secrets at rest.

### Backup Procedure

```bash
zcplatform backup --output /var/backups/zcplatform/
# Produces: zcplatform-backup-YYYY-MM-DD-HHMMSS.tar.gz
```

### Restore Procedure

```bash
# Stop the platform first
systemctl stop zcplatform

zcplatform restore --archive /var/backups/zcplatform/zcplatform-backup-2026-02-21-030000.tar.gz

systemctl start zcplatform
```

The restore command overwrites the current DB and vault key. Confirm the archive is intact before restoring on a live node.

### Recommended Cron Schedule

```cron
# Daily backup at 03:00, keep 14 days
0 3 * * * zcplatform backup --output /var/backups/zcplatform/ && \
  find /var/backups/zcplatform/ -name "*.tar.gz" -mtime +14 -delete
```

For critical deployments, copy archives off-host (S3, rsync to secondary) immediately after creation.

---

## Key Rotation

### When to Rotate

- Suspected vault key compromise.
- Staff/operator turnover for high-trust environments.
- Scheduled policy rotation (e.g., every 90 days).

### How to Rotate

```bash
zcplatform rotate-key
```

This appends a new key version to the vault. Existing secrets remain readable (old key versions are retained for decryption). New secrets are written with the latest key. Old key versions are not automatically purged — maintain key history in the vault file.

### Backward Compatibility

Rotation is non-destructive. Running tenants do not need to be stopped. The vault file grows by one key entry per rotation; this is negligible in size.

---

## Database Maintenance

### WAL Checkpointing

SQLite WAL mode is enabled by default. The WAL file is checkpointed automatically, but for long-running instances:

```bash
# Force checkpoint (safe on live instance)
sqlite3 zcplatform.db "PRAGMA wal_checkpoint(TRUNCATE);"
```

### Reclaim Space After Deletions

```bash
# Run after bulk tenant deletions or metric pruning
sqlite3 zcplatform.db "VACUUM;"
```

### Monitor DB Size

```bash
du -sh zcplatform.db zcplatform.db-wal zcplatform.db-shm
```

The `resource_snapshots` table is pruned to 7-day retention automatically. If the DB grows unexpectedly, check audit log volume and adjust retention policy.

---

## Scaling Strategies

### Vertical (Immediate)

Upgrade the VPS. Memory is the primary constraint. Doubling RAM roughly doubles tenant capacity. No configuration change required.

### Horizontal (Future)

Migrate the backing store to PostgreSQL with a shared connection pool. Each zcplatform node connects to the same DB. Requires a future database adapter abstraction in the platform codebase.

### Sharding (Future)

Deploy multiple zcplatform nodes, each managing a subset of tenants. Use a DNS or L4 load balancer to route tenant subdomains to the correct node. Tenant assignment is static; no cross-node migration tooling exists yet.

---

## Alerting Recommendations

Configure external monitoring (Uptime Robot, Grafana, Prometheus Alertmanager) against these signals:

| Signal | Threshold | Severity |
|--------|-----------|----------|
| `health_checker` error count | >0 for >5 min | Warning |
| `resource_snapshots.cpu_pct` sustained | >80% for >15 min | Warning |
| Host disk usage | >80% | Warning |
| Host disk usage | >95% | Critical |
| Platform API `/api/monitoring/health` unreachable | >2 min | Critical |
| Backup job exit code | non-zero | Warning |

---

## Log Management

### Container Logs

Docker captures stdout/stderr from each agent container. By default, logs use the `json-file` driver with no rotation limit. Set a cap in `/etc/docker/daemon.json`:

```json
{
  "log-driver": "json-file",
  "log-opts": {
    "max-size": "50m",
    "max-file": "3"
  }
}
```

Restart Docker after changing: `systemctl restart docker`.

### Platform Logs

If zcplatform is managed by systemd:

```bash
journalctl -u zcplatform -f
journalctl -u zcplatform --since "1 hour ago"
```

Set `SystemMaxUse=500M` in `/etc/systemd/journald.conf` to bound journal disk usage.
