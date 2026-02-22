use crate::db::pool::DbPool;
use crate::docker::DockerManager;
use crate::tenant::filesystem;
use std::time::Duration;
use tokio::sync::broadcast;
use tokio::time::interval;
use tracing::{error, info, warn};
use uuid::Uuid;

const COLLECT_INTERVAL_SECS: u64 = 60;
const RETENTION_DAYS: i64 = 7;

pub async fn run(db: DbPool, data_dir: String, mut shutdown_rx: broadcast::Receiver<()>) {
    let mut ticker = interval(Duration::from_secs(COLLECT_INTERVAL_SECS));

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                collect_all(&db, &data_dir).await;
                purge_old(&db);
            }
            _ = shutdown_rx.recv() => {
                info!("resource_collector: shutdown signal received");
                break;
            }
        }
    }
}

async fn collect_all(db: &DbPool, data_dir: &str) {
    let tenants: Vec<(String, String)> = match db.read(|conn| {
        let mut stmt = conn.prepare("SELECT id, slug FROM tenants WHERE status = 'running'")?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }) {
        Ok(t) => t,
        Err(e) => {
            error!("resource_collector: failed to load tenants: {}", e);
            return;
        }
    };

    tracing::debug!(
        "resource_collector: collecting for {} tenants",
        tenants.len()
    );

    for (tenant_id, slug) in tenants {
        collect_tenant(db, data_dir, &tenant_id, &slug).await;
    }
}

async fn collect_tenant(db: &DbPool, data_dir: &str, tenant_id: &str, slug: &str) {
    let slug_clone = slug.to_string();
    let data_dir_clone = data_dir.to_string();
    let slug_for_disk = slug.to_string();

    let stats_result =
        tokio::task::spawn_blocking(move || DockerManager::container_stats(&slug_clone)).await;

    let disk_result = tokio::task::spawn_blocking(move || {
        filesystem::tenant_disk_usage(&data_dir_clone, &slug_for_disk)
    })
    .await;

    let stats = match stats_result {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => {
            warn!("resource_collector: stats failed for {}: {}", slug, e);
            return;
        }
        Err(e) => {
            warn!(
                "resource_collector: spawn_blocking failed for {}: {}",
                slug, e
            );
            return;
        }
    };

    let disk_bytes = match disk_result {
        Ok(Ok(d)) => d,
        Ok(Err(e)) => {
            warn!("resource_collector: disk usage failed for {}: {}", slug, e);
            0
        }
        Err(_) => 0,
    };

    let id = Uuid::new_v4().to_string();

    if let Err(e) = db.write(|conn| {
        conn.execute(
            "INSERT INTO resource_snapshots \
             (id, tenant_id, cpu_pct, mem_bytes, mem_limit, disk_bytes, net_in_bytes, net_out_bytes, pids) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                id,
                tenant_id,
                stats.cpu_pct,
                stats.mem_bytes as i64,
                stats.mem_limit as i64,
                disk_bytes as i64,
                stats.net_in_bytes as i64,
                stats.net_out_bytes as i64,
                stats.pids
            ],
        )?;
        Ok(())
    }) {
        error!(
            "resource_collector: insert failed for {}: {}",
            slug, e
        );
    }
}

fn purge_old(db: &DbPool) {
    if let Err(e) = db.write(|conn| {
        let deleted = conn.execute(
            &format!(
                "DELETE FROM resource_snapshots WHERE ts < datetime('now', '-{} days')",
                RETENTION_DAYS
            ),
            [],
        )?;
        if deleted > 0 {
            tracing::debug!("resource_collector: purged {} old snapshots", deleted);
        }
        Ok(())
    }) {
        error!("resource_collector: purge failed: {}", e);
    }
}
