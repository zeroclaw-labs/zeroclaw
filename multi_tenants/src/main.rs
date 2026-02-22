use anyhow::Context;
use clap::{Parser, Subcommand};
use std::net::SocketAddr;
use tracing::info;
use zcplatform::{
    auth::{jwt::JwtService, rate_limit::RateLimiter},
    background::{self, BackgroundCoordinator},
    config,
    db::{self, pool::DbPool},
    docker::DockerManager,
    proxy::ProxyManager,
    state::AppState,
    tenant::provisioner::Provisioner,
    vault::{key as vault_key, VaultService},
};

// ── CLI definition ─────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(name = "zcplatform", about = "ZeroClaw Multi-Tenant Platform", version)]
struct Cli {
    /// Path to TOML config file
    #[arg(short, long, default_value = "platform.toml")]
    config: String,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Initialise data directories, master key, database and super-admin user
    Bootstrap {
        /// Super-admin email address
        #[arg(long, default_value = "admin@localhost")]
        admin_email: String,
    },
    /// Start the HTTP API server
    Serve,
    /// Backup database, keys, and tenant data
    Backup {
        /// Output directory for backup files
        #[arg(long, default_value = "data/backups")]
        output: String,
    },
    /// Restore platform from a backup archive
    Restore {
        /// Path to backup archive (.tar.gz)
        #[arg(long)]
        from: String,
    },
    /// Rotate vault encryption key and re-encrypt all secrets
    RotateKey,
    /// Build tenant Docker image with version tag
    BuildImage {
        /// Version tag (e.g., "0.2.0")
        #[arg(long)]
        version: String,
    },
}

// ── Entry point ────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialise structured logging from RUST_LOG (default: info)
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "zcplatform=info".into()),
        )
        .init();

    let cli = Cli::parse();
    let cfg = config::load(&cli.config)?;

    match cli.command {
        Commands::Bootstrap { admin_email } => bootstrap(cfg, &admin_email).await,
        Commands::Serve => serve(cfg).await,
        Commands::Backup { output } => backup(cfg, &output).await,
        Commands::Restore { from } => restore(cfg, &from).await,
        Commands::RotateKey => rotate_key(cfg).await,
        Commands::BuildImage { version } => build_image(cfg, &version).await,
    }
}

// ── Bootstrap ──────────────────────────────────────────────────────────────

async fn bootstrap(cfg: config::PlatformConfig, admin_email: &str) -> anyhow::Result<()> {
    info!("Starting bootstrap...");

    // 1. Create required data directories
    create_data_dirs(&cfg)?;

    // 2. Generate master key if the key file does not yet exist
    if !cfg.master_key_path.exists() {
        let version = vault_key::generate_key(&cfg.master_key_path)
            .context("failed to generate master key")?;
        info!("Generated master key (version: {})", version);
    } else {
        info!("Master key file already exists, skipping generation");
    }

    // 3. Load vault from key file
    let (current_version, keys) =
        vault_key::load_keys(&cfg.master_key_path).context("failed to load vault keys")?;
    let vault = VaultService::new(current_version, keys);
    info!(
        "Vault ready (current key version: {})",
        vault.current_version()
    );

    // 4. Open DB (single writer is sufficient for bootstrap) and run migrations
    let db_path = cfg
        .database_path
        .to_str()
        .context("database_path is not valid UTF-8")?;
    let db = DbPool::open(db_path, 1).context("failed to open database")?;
    db::run_migrations(&db).context("failed to run database migrations")?;
    info!("Database migrations applied");

    // 5. Create super-admin user (idempotent — skipped if already exists)
    let created = create_super_admin_if_absent(&db, admin_email)
        .context("failed to create super-admin user")?;
    if created {
        info!("Super-admin created: {}", admin_email);
    } else {
        info!("Super-admin already exists: {}", admin_email);
    }

    // 6. Record vault key version in DB for rotation tracking
    set_db_meta(
        &db,
        "vault_key_version",
        &vault.current_version().to_string(),
    )
    .context("failed to persist vault key version")?;

    // 7. Verify Docker is available
    match zcplatform::docker::DockerManager::health_check() {
        Ok(true) => info!("Docker daemon is reachable"),
        Ok(false) => {
            tracing::warn!("Docker daemon not responding — container features will not work")
        }
        Err(e) => tracing::warn!(
            "Docker check failed: {} — container features will not work",
            e
        ),
    }

    // 8. Create Docker networks (only if Docker is available)
    if zcplatform::docker::DockerManager::health_check().unwrap_or(false) {
        if let Err(e) = zcplatform::docker::network::ensure_network(&cfg.docker_network) {
            tracing::warn!("Failed to create internal network: {}", e);
        }
        if let Err(e) = zcplatform::docker::network::ensure_external_network("zcplatform-external")
        {
            tracing::warn!("Failed to create external network: {}", e);
        }
    }

    info!("Bootstrap complete.");
    Ok(())
}

// ── Serve ──────────────────────────────────────────────────────────────────

async fn serve(cfg: config::PlatformConfig) -> anyhow::Result<()> {
    info!("Loading vault keys...");
    let (current_version, keys) = vault_key::load_keys(&cfg.master_key_path)
        .context("failed to load vault keys — run `bootstrap` first")?;
    let vault = VaultService::new(current_version, keys);
    info!("Vault ready (key version: {})", vault.current_version());

    info!("Opening database (4 reader connections)...");
    let db_path = cfg
        .database_path
        .to_str()
        .context("database_path is not valid UTF-8")?;
    let db = DbPool::open(db_path, 4).context("failed to open database")?;

    // JWT service — load from config, persistent file, or generate-and-save
    let jwt_secret = if let Some(ref secret) = cfg.jwt_secret {
        secret.clone()
    } else {
        let secret_path = cfg.data_dir.join("jwt_secret.key");
        if secret_path.exists() {
            std::fs::read_to_string(&secret_path)
                .context("failed to read jwt_secret.key")?
                .trim()
                .to_string()
        } else {
            tracing::info!(
                "Generating new JWT secret (persisting to {:?})",
                secret_path
            );
            let mut secret_bytes = [0u8; 32];
            rand::RngExt::fill(&mut rand::rng(), &mut secret_bytes);
            let secret = hex::encode(secret_bytes);
            std::fs::create_dir_all(&cfg.data_dir).context("failed to create data directory")?;
            std::fs::write(&secret_path, &secret).context("failed to write jwt_secret.key")?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&secret_path, std::fs::Permissions::from_mode(0o600))?;
            }
            secret
        }
    };
    let jwt = JwtService::new(&jwt_secret);

    // OTP rate limiter: 5 attempts per email per 15 minutes
    let otp_limiter = RateLimiter::new(900, 5);

    // Docker manager
    let data_dir = cfg.data_dir.to_str().unwrap_or("data/tenants");
    let docker = DockerManager::new(data_dir, &cfg.docker_network, &cfg.docker_image);

    // Tenant provisioner
    let provisioner = Provisioner::new(
        data_dir,
        cfg.port_range,
        cfg.uid_range,
        "http://zcplatform-egress-proxy:8888",
    );

    // Caddy proxy manager (only if domain is configured)
    let proxy = cfg.domain.as_ref().map(|domain| {
        info!("Proxy manager enabled for domain: {}", domain);
        ProxyManager::new(&cfg.caddy_api_url, domain)
    });

    // Email service (only if SMTP is configured)
    let email = cfg.smtp.as_ref().and_then(|smtp_cfg| {
        match zcplatform::email::EmailService::new(smtp_cfg) {
            Ok(svc) => {
                info!(
                    "Email service ready (SMTP: {}:{})",
                    smtp_cfg.host, smtp_cfg.port
                );
                Some(svc)
            }
            Err(e) => {
                tracing::warn!(
                    "SMTP not available: {} — OTP codes will be logged to console",
                    e
                );
                None
            }
        }
    });

    let state = AppState::new(
        cfg.clone(),
        db,
        vault,
        jwt,
        otp_limiter,
        docker,
        provisioner,
        proxy,
        email,
    );

    // Spawn rate limiter cleanup (every 5 minutes)
    let limiter_state = state.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(300));
        loop {
            interval.tick().await;
            limiter_state.otp_limiter.cleanup();
        }
    });
    tracing::info!("Background: rate limiter cleanup started (5min interval)");

    // Initialize Caddy config and sync tenant routes (if proxy is configured)
    if let Some(ref proxy_mgr) = state.proxy {
        // Load base Caddy config (platform routes + server settings)
        if let Err(e) = proxy_mgr.init_config(cfg.port).await {
            tracing::warn!(
                "Failed to init Caddy config: {} — routes will not work until Caddy is available",
                e
            );
        } else {
            info!("Caddy base config loaded");

            // Sync routes for running tenants
            let running_tenants: Vec<(String, u16)> = state.db.read(|conn| {
                let mut stmt =
                    conn.prepare("SELECT slug, port FROM tenants WHERE status = 'running'")?;
                let rows = stmt
                    .query_map([], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, u16>(1)?))
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(rows)
            })?;

            if !running_tenants.is_empty() {
                if let Err(e) = proxy_mgr.sync_routes(&running_tenants).await {
                    tracing::warn!("Failed to sync Caddy routes: {}", e);
                } else {
                    info!("Synced {} tenant routes to Caddy", running_tenants.len());
                }
            }
        }
    }

    // Background task coordinator
    let coordinator = BackgroundCoordinator::new();

    // Spawn health checker (owns its own DB + Docker connections)
    let hc_db = DbPool::open(db_path, 1).context("failed to open health-checker DB pool")?;
    let hc_docker = DockerManager::new(data_dir, &cfg.docker_network, &cfg.docker_image);
    let hc_shutdown = coordinator.subscribe_shutdown();
    tokio::spawn(async move {
        background::health_checker::run(hc_db, hc_docker, hc_shutdown).await;
    });
    info!("Background: health checker started (30s interval)");

    // Spawn usage collector (owns its own DB connection)
    let uc_db = DbPool::open(db_path, 1).context("failed to open usage-collector DB pool")?;
    let uc_shutdown = coordinator.subscribe_shutdown();
    tokio::spawn(async move {
        background::usage_collector::run(uc_db, uc_shutdown).await;
    });
    info!("Background: usage collector started (5min interval)");

    // Spawn resource collector (owns its own DB connection)
    let rc_db = DbPool::open(db_path, 1).context("failed to open resource-collector DB pool")?;
    let rc_shutdown = coordinator.subscribe_shutdown();
    let rc_data_dir = data_dir.to_string();
    tokio::spawn(async move {
        background::resource_collector::run(rc_db, rc_data_dir, rc_shutdown).await;
    });
    info!("Background: resource collector started (60s interval)");

    let app = zcplatform::routes::app(state);

    let addr: SocketAddr = format!("{}:{}", cfg.host, cfg.port)
        .parse()
        .context("invalid bind address")?;

    info!("Listening on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .context("failed to bind TCP listener")?;

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("server error")?;

    // Signal background tasks to stop
    coordinator.shutdown();
    info!("Background tasks signalled to stop.");

    info!("Server stopped.");
    Ok(())
}

// ── Graceful shutdown ──────────────────────────────────────────────────────

async fn shutdown_signal() {
    tokio::signal::ctrl_c()
        .await
        .expect("failed to install CTRL+C handler");
    info!("Shutdown signal received, stopping server...");
}

// ── Bootstrap helpers ──────────────────────────────────────────────────────

fn create_data_dirs(cfg: &config::PlatformConfig) -> anyhow::Result<()> {
    // Parent directory of the database file
    if let Some(parent) = cfg.database_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create db dir: {}", parent.display()))?;
        }
    }

    // Parent directory of the master key file
    if let Some(parent) = cfg.master_key_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create key dir: {}", parent.display()))?;
        }
    }

    // Tenant data directory
    std::fs::create_dir_all(&cfg.data_dir)
        .with_context(|| format!("failed to create data dir: {}", cfg.data_dir.display()))?;

    info!(
        "Data directories ready: db={}, key={}, tenants={}",
        cfg.database_path.display(),
        cfg.master_key_path.display(),
        cfg.data_dir.display()
    );
    Ok(())
}

/// Returns `true` if a new super-admin row was inserted, `false` if one already existed.
///
/// Note: The `users` table (created by migration 001) has no `password_hash` column;
/// authentication credentials are managed by the auth/OTP module.  Bootstrap only
/// creates the user identity row with `is_super_admin = 1`.
fn create_super_admin_if_absent(db: &DbPool, email: &str) -> anyhow::Result<bool> {
    db.write(|conn| {
        let exists: bool = conn.query_row(
            "SELECT COUNT(*) > 0 FROM users WHERE email = ?1",
            [email],
            |row| row.get(0),
        )?;

        if exists {
            return Ok(false);
        }

        conn.execute(
            "INSERT INTO users (id, email, is_super_admin)
             VALUES (lower(hex(randomblob(16))), ?1, 1)",
            [email],
        )?;
        Ok(true)
    })
}

/// Record the current vault key version in the `vault_keys` metadata table (idempotent).
fn set_db_meta(db: &DbPool, _key: &str, value: &str) -> anyhow::Result<()> {
    let version: u32 = value
        .parse()
        .context("vault key version must be a valid u32")?;

    db.write(|conn| {
        // vault_keys tracks each key version that has been generated.
        // Insert is a no-op if this version was already recorded.
        conn.execute(
            "INSERT OR IGNORE INTO vault_keys (version) VALUES (?1)",
            [version],
        )?;
        Ok(())
    })
}

// ── Backup ────────────────────────────────────────────────────────────────

async fn backup(cfg: config::PlatformConfig, output_dir: &str) -> anyhow::Result<()> {
    info!("Starting backup...");
    std::fs::create_dir_all(output_dir)?;

    let timestamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    let backup_dir = format!("{}/backup-{}", output_dir, timestamp);
    std::fs::create_dir_all(&backup_dir)?;

    // 1. SQLite online backup (no downtime, safe with WAL)
    info!("Backing up database...");
    let db_backup_path = format!("{}/platform.db", backup_dir);
    let source = rusqlite::Connection::open(&cfg.database_path)?;
    let mut dest = rusqlite::Connection::open(&db_backup_path)?;
    let backup = rusqlite::backup::Backup::new(&source, &mut dest)?;
    backup.run_to_completion(100, std::time::Duration::from_millis(50), None)?;
    drop(backup);
    drop(dest);
    drop(source);
    info!("Database backed up");

    // 2. Copy master key file
    info!("Backing up key file...");
    let key_backup = format!("{}/master.key", backup_dir);
    std::fs::copy(&cfg.master_key_path, &key_backup)?;

    // 3. Archive tenant data directories
    info!("Archiving tenant data...");
    let tenant_archive = format!("{}/tenants.tar.gz", backup_dir);
    let data_dir_str = cfg.data_dir.to_string_lossy();
    let status = std::process::Command::new("tar")
        .args(["-czf", &tenant_archive, "-C", &data_dir_str, "."])
        .status()
        .context("failed to run tar")?;
    if !status.success() {
        anyhow::bail!("tenant data archive failed");
    }

    // 4. Create final archive
    info!("Creating final archive...");
    let final_archive = format!("{}/zcplatform-backup-{}.tar.gz", output_dir, timestamp);
    let backup_name = format!("backup-{}", timestamp);
    let status = std::process::Command::new("tar")
        .args(["-czf", &final_archive, "-C", output_dir, &backup_name])
        .status()
        .context("failed to create final archive")?;

    if status.success() {
        std::fs::remove_dir_all(&backup_dir)?;
        info!("Backup complete: {}", final_archive);
    } else {
        anyhow::bail!("final archive creation failed");
    }

    Ok(())
}

// ── Restore ───────────────────────────────────────────────────────────────

async fn restore(cfg: config::PlatformConfig, archive_path: &str) -> anyhow::Result<()> {
    if !std::path::Path::new(archive_path).exists() {
        anyhow::bail!("archive not found: {}", archive_path);
    }

    info!("Starting restore from {}...", archive_path);

    // 1. Stop all tenant containers
    tracing::warn!("Stopping all tenant containers...");
    let output = std::process::Command::new("docker")
        .args(["ps", "-q", "--filter", "name=zc-tenant-"])
        .output()
        .context("failed to list containers")?;
    let container_ids = String::from_utf8_lossy(&output.stdout);
    for id in container_ids.lines() {
        let id = id.trim();
        if !id.is_empty() {
            let _ = std::process::Command::new("docker")
                .args(["stop", "-t", "5", id])
                .status();
        }
    }

    // 2. Extract archive to temp dir
    let temp_dir = format!("/tmp/zcplatform-restore-{}", uuid::Uuid::new_v4());
    std::fs::create_dir_all(&temp_dir)?;
    let status = std::process::Command::new("tar")
        .args(["-xzf", archive_path, "-C", &temp_dir])
        .status()
        .context("failed to extract archive")?;
    if !status.success() {
        std::fs::remove_dir_all(&temp_dir)?;
        anyhow::bail!("archive extraction failed");
    }

    // 3. Find the backup subdirectory
    let entries: Vec<_> = std::fs::read_dir(&temp_dir)?
        .filter_map(|e| e.ok())
        .collect();
    let backup_dir = if entries.len() == 1 && entries[0].path().is_dir() {
        entries[0].path()
    } else {
        std::path::PathBuf::from(&temp_dir)
    };

    // 4. Restore database
    let db_path = backup_dir.join("platform.db");
    if db_path.exists() {
        std::fs::copy(&db_path, &cfg.database_path).context("failed to restore database")?;
        info!("Database restored");
    } else {
        tracing::warn!("No database file in backup, skipping");
    }

    // 5. Restore key file
    let key_path = backup_dir.join("master.key");
    if key_path.exists() {
        std::fs::copy(&key_path, &cfg.master_key_path).context("failed to restore key file")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&cfg.master_key_path, std::fs::Permissions::from_mode(0o600))?;
        }
        info!("Key file restored");
    } else {
        tracing::warn!("No key file in backup, skipping");
    }

    // 6. Restore tenant data
    let tenant_archive = backup_dir.join("tenants.tar.gz");
    if tenant_archive.exists() {
        std::fs::create_dir_all(&cfg.data_dir)?;
        let data_dir_str = cfg.data_dir.to_string_lossy();
        let archive_str = tenant_archive.to_string_lossy();
        let status = std::process::Command::new("tar")
            .args(["-xzf", &*archive_str, "-C", &*data_dir_str])
            .status()
            .context("failed to extract tenant data")?;
        if !status.success() {
            anyhow::bail!("tenant data restore failed");
        }
        info!("Tenant data restored");
    }

    // 7. Cleanup
    std::fs::remove_dir_all(&temp_dir)?;

    info!("Restore complete. Start zcplatform to resume operations.");
    Ok(())
}

// ── Key Rotation ──────────────────────────────────────────────────────────

async fn rotate_key(cfg: config::PlatformConfig) -> anyhow::Result<()> {
    info!("Starting key rotation...");

    // 1. Generate new key version (appends to key file)
    let new_version =
        vault_key::generate_key(&cfg.master_key_path).context("failed to generate new key")?;
    info!("Generated new key version: {}", new_version);

    // 2. Load all keys (including the new one)
    let (_, keys) = vault_key::load_keys(&cfg.master_key_path).context("failed to load keys")?;
    let vault = VaultService::new(new_version, keys);

    // 3. Open DB
    let db_path = cfg.database_path.to_str().context("invalid db path")?;
    let pool = db::pool::DbPool::open(db_path, 1)?;

    // 4. Re-encrypt tenant API keys
    let configs: Vec<(String, String)> = pool.read(|conn| {
        let mut stmt = conn.prepare("SELECT tenant_id, api_key_enc FROM tenant_configs WHERE api_key_enc IS NOT NULL AND api_key_enc != ''")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?.collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    })?;

    let mut re_encrypted_count = 0u32;
    for (tenant_id, encrypted) in &configs {
        let plaintext = vault
            .decrypt(encrypted)
            .with_context(|| format!("failed to decrypt API key for tenant {}", tenant_id))?;
        let re_encrypted = vault.encrypt(&plaintext)?;
        pool.write(|conn| {
            conn.execute(
                "UPDATE tenant_configs SET api_key_enc = ?1 WHERE tenant_id = ?2",
                rusqlite::params![re_encrypted, tenant_id],
            )?;
            Ok(())
        })?;
        re_encrypted_count += 1;
    }
    info!("Re-encrypted {} tenant API keys", re_encrypted_count);

    // 5. Re-encrypt channel configs
    let channels: Vec<(String, String)> = pool.read(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, config_enc FROM channels WHERE config_enc IS NOT NULL AND config_enc != ''",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    })?;

    let mut channel_count = 0u32;
    for (channel_id, encrypted) in &channels {
        let plaintext = vault
            .decrypt(encrypted)
            .with_context(|| format!("failed to decrypt config for channel {}", channel_id))?;
        let re_encrypted = vault.encrypt(&plaintext)?;
        pool.write(|conn| {
            conn.execute(
                "UPDATE channels SET config_enc = ?1 WHERE id = ?2",
                rusqlite::params![re_encrypted, channel_id],
            )?;
            Ok(())
        })?;
        channel_count += 1;
    }
    info!("Re-encrypted {} channel configs", channel_count);

    // 6. Record key version in DB
    pool.write(|conn| {
        conn.execute(
            "INSERT OR IGNORE INTO vault_keys (version) VALUES (?1)",
            [new_version],
        )?;
        Ok(())
    })?;

    info!("Key rotation complete. Previous versions retained for decryption.");
    Ok(())
}

// ── Build Image ───────────────────────────────────────────────────────────

async fn build_image(_cfg: config::PlatformConfig, version: &str) -> anyhow::Result<()> {
    info!("Building tenant image version: {}", version);

    let dockerfile = std::path::Path::new("platform/docker/Dockerfile.tenant");
    let context = std::path::Path::new("platform/docker");

    // Copy latest zeroclaw binary into build context
    let binary_src = std::path::Path::new("target/release/zeroclaw");
    let binary_dst = context.join("zeroclaw");
    if binary_src.exists() {
        std::fs::copy(binary_src, &binary_dst)
            .context("failed to copy zeroclaw binary to build context")?;
    } else {
        anyhow::bail!(
            "zeroclaw binary not found at {}. Run `cargo build --release` first.",
            binary_src.display()
        );
    }

    zcplatform::docker::image::build_tenant_image(dockerfile, context, version)?;

    // Cleanup
    let _ = std::fs::remove_file(&binary_dst);

    info!(
        "Built and tagged: zeroclaw-tenant:{} + zeroclaw-tenant:latest",
        version
    );
    Ok(())
}
