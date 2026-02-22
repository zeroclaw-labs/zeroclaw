use crate::db::pool::DbPool;
use crate::docker::DockerManager;
use crate::proxy::ProxyManager;
use crate::tenant::{allocator, config_render, filesystem, health, pairing, slug};
use crate::vault::VaultService;
use anyhow::{bail, Result};

pub struct Provisioner {
    pub data_dir: String,
    pub port_range: [u16; 2],
    pub uid_range: [u32; 2],
    pub egress_proxy_url: String,
}

pub struct CreateTenantInput {
    pub name: String,
    /// Super-admin override for slug. When `None`, slug is auto-generated.
    pub custom_slug: Option<String>,
    pub plan: String,
    pub api_key: String,
    pub provider: String,
    pub model: String,
    pub temperature: f64,
    pub autonomy_level: String,
    pub system_prompt: Option<String>,
}

/// Minimal input for two-phase creation (name+plan only, no provider/container).
pub struct CreateDraftInput {
    pub name: String,
    pub custom_slug: Option<String>,
    pub plan: String,
}

impl Provisioner {
    pub fn new(
        data_dir: &str,
        port_range: [u16; 2],
        uid_range: [u32; 2],
        egress_proxy_url: &str,
    ) -> Self {
        Self {
            data_dir: data_dir.to_string(),
            port_range,
            uid_range,
            egress_proxy_url: egress_proxy_url.to_string(),
        }
    }

    /// Create a new tenant end-to-end.
    ///
    /// Steps:
    /// 1. Encrypt API key (CPU work, outside transaction)
    /// 2. Generate/validate slug, allocate port+UID, INSERT tenant+configs atomically
    /// 3. Create filesystem dirs
    /// 4. Write config.toml
    /// 5. Build env vars
    /// 6. Derive plan limits
    /// 7. Create and start container
    /// 8. Update DB with container_id
    /// 9. Health poll (30s)
    /// 10. Update status to "running" or "error"
    /// 11. If unhealthy: stop orphaned container (H3)
    /// 12. Add Caddy proxy route on success (M12)
    ///
    /// Returns `(tenant_id, slug)`.
    /// On failure, rolls back DB records and filesystem.
    pub async fn create_tenant(
        &self,
        db: &DbPool,
        docker: &DockerManager,
        vault: &VaultService,
        proxy: Option<&ProxyManager>,
        input: CreateTenantInput,
    ) -> Result<(String, String)> {
        // 1. Encrypt API key outside transaction (CPU work)
        let encrypted_key = vault.encrypt(&input.api_key)?;

        // 2. Slug, port, UID allocation + INSERT in a single write transaction (fixes H2 TOCTOU)
        let port_range = self.port_range;
        let uid_range = self.uid_range;
        let name = input.name.clone();
        let plan = input.plan.clone();
        let custom_slug_opt = input.custom_slug.clone();
        let provider = input.provider.clone();
        let model = input.model.clone();
        let temp = input.temperature;
        let autonomy = input.autonomy_level.clone();
        let system_prompt = input.system_prompt.clone();
        let enc_key = encrypted_key.clone();

        let (tenant_id, slug, port, uid) = db.write(move |conn| {
            // Generate or validate slug inside transaction (WP-3d)
            let resolved_slug = match &custom_slug_opt {
                Some(s) => {
                    let exists: bool = conn.query_row(
                        "SELECT EXISTS(SELECT 1 FROM tenants WHERE slug = ?1)",
                        [s.as_str()],
                        |row| row.get(0),
                    )?;
                    if exists {
                        anyhow::bail!("tenant slug '{}' already exists", s);
                    }
                    s.clone()
                }
                None => slug::generate_unique_slug(conn, &name)?,
            };

            // Allocate port and UID inside same transaction (H2: eliminates TOCTOU race)
            let port = allocator::allocate_port(conn, port_range)?;
            let uid = allocator::allocate_uid(conn, uid_range)?;
            let tenant_id = uuid::Uuid::new_v4().to_string();

            conn.execute(
                "INSERT INTO tenants (id, name, slug, status, plan, port, uid)
                 VALUES (?1, ?2, ?3, 'provisioning', ?4, ?5, ?6)",
                rusqlite::params![
                    tenant_id,
                    name,
                    resolved_slug,
                    plan,
                    port as i64,
                    uid as i64,
                ],
            )?;

            // C1: correct column names matching actual schema
            conn.execute(
                "INSERT INTO tenant_configs
                    (tenant_id, api_key_enc, provider, model, temperature,
                     autonomy_level, system_prompt)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![
                    tenant_id,
                    enc_key,
                    provider,
                    model,
                    temp,
                    autonomy,
                    system_prompt,
                ],
            )?;

            Ok((tenant_id, resolved_slug, port, uid))
        })?;

        // 3. Create filesystem dirs
        if let Err(e) = filesystem::create_tenant_dirs(&self.data_dir, &slug, uid) {
            tracing::error!("filesystem setup failed for {}: {}", slug, e);
            self.rollback_db(db, &tenant_id)?;
            bail!("filesystem setup failed: {}", e);
        }

        // 4. Write config.toml
        let config_content = config_render::render_config_toml(
            &input.autonomy_level,
            input.system_prompt.as_deref(),
            &self.egress_proxy_url,
        );
        if let Err(e) = filesystem::write_tenant_config(&self.data_dir, &slug, &config_content, uid)
        {
            tracing::error!("config write failed for {}: {}", slug, e);
            self.rollback_db(db, &tenant_id)?;
            self.rollback_filesystem(&slug)?;
            bail!("config write failed: {}", e);
        }

        // 5. Build env vars
        let env_vars_owned =
            config_render::build_env_vars(&input.api_key, &input.provider, &input.model, port);
        let env_refs: Vec<(&str, &str)> = env_vars_owned
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();

        // 6. Derive plan limits
        let (memory_mb, cpu_limit) = match input.plan.as_str() {
            "pro" => (512u32, 1.0f64),
            _ => (256u32, 0.5f64), // free / default
        };

        // 7. Create and start container
        let container_id =
            match docker.create_container(&slug, port, uid, &env_refs, memory_mb, cpu_limit) {
                Ok(id) => id,
                Err(e) => {
                    tracing::error!("docker create failed for {}: {}", slug, e);
                    self.rollback_db(db, &tenant_id)?;
                    self.rollback_filesystem(&slug)?;
                    bail!("container creation failed: {}", e);
                }
            };

        // 8. Update DB with container_id
        db.write(|conn| {
            conn.execute(
                "UPDATE tenants SET container_id = ?1 WHERE id = ?2",
                rusqlite::params![container_id, tenant_id],
            )?;
            Ok(())
        })?;

        // 9. Health poll (30s timeout)
        let healthy = health::poll_health(port, 30).await.unwrap_or(false);

        // 10. Update status
        let final_status = if healthy { "running" } else { "error" };
        db.write(|conn| {
            conn.execute(
                "UPDATE tenants SET status = ?1 WHERE id = ?2",
                rusqlite::params![final_status, tenant_id],
            )?;
            Ok(())
        })?;

        // 11. If unhealthy: warn and stop orphaned container (H3)
        if !healthy {
            tracing::warn!(
                "tenant {} provisioned but health check timed out; status=error",
                slug
            );
            if let Err(e) = docker.stop_container(&slug) {
                tracing::warn!("failed to stop unhealthy container {}: {}", slug, e);
            }
        }

        // 11b. Read and store pairing code (after health check passes)
        if healthy {
            if let Err(e) = pairing::read_and_store_pairing_code(docker, db, &tenant_id, &slug).await {
                tracing::warn!("failed to read pairing code for {}: {}", slug, e);
            }
        }

        // 12. Add Caddy proxy route on success (M12)
        if final_status == "running" {
            if let Some(proxy) = proxy {
                if let Err(e) = proxy.add_route(&slug, port).await {
                    tracing::warn!("failed to add Caddy route for {}: {}", slug, e);
                }
            }
        }

        Ok((tenant_id, slug))
    }

    /// Create a draft tenant (name+plan only, no container).
    ///
    /// Allocates port + UID and inserts DB records with status='draft'.
    /// Config row is created with placeholder values — caller should PATCH /config before deploy.
    /// Returns `(tenant_id, slug)`.
    pub fn create_draft(
        &self,
        db: &DbPool,
        vault: &VaultService,
        input: CreateDraftInput,
    ) -> Result<(String, String)> {
        // Encrypt a placeholder key so the column is never NULL
        let placeholder_enc = vault.encrypt("__pending__")?;

        let port_range = self.port_range;
        let uid_range = self.uid_range;
        let name = input.name.clone();
        let plan = input.plan.clone();
        let custom_slug_opt = input.custom_slug.clone();
        let enc_key = placeholder_enc;

        let (tenant_id, resolved_slug) = db.write(move |conn| {
            let resolved_slug = match &custom_slug_opt {
                Some(s) => {
                    let exists: bool = conn.query_row(
                        "SELECT EXISTS(SELECT 1 FROM tenants WHERE slug = ?1)",
                        [s.as_str()],
                        |row| row.get(0),
                    )?;
                    if exists {
                        anyhow::bail!("tenant slug '{}' already exists", s);
                    }
                    s.clone()
                }
                None => slug::generate_unique_slug(conn, &name)?,
            };

            let port = allocator::allocate_port(conn, port_range)?;
            let uid = allocator::allocate_uid(conn, uid_range)?;
            let tenant_id = uuid::Uuid::new_v4().to_string();

            conn.execute(
                "INSERT INTO tenants (id, name, slug, status, plan, port, uid)
                 VALUES (?1, ?2, ?3, 'draft', ?4, ?5, ?6)",
                rusqlite::params![
                    tenant_id,
                    name,
                    resolved_slug,
                    plan,
                    port as i64,
                    uid as i64
                ],
            )?;

            conn.execute(
                "INSERT INTO tenant_configs
                    (tenant_id, api_key_enc, provider, model, temperature,
                     autonomy_level, system_prompt)
                 VALUES (?1, ?2, '', '', 0.7, 'supervised', NULL)",
                rusqlite::params![tenant_id, enc_key],
            )?;

            Ok((tenant_id, resolved_slug))
        })?;

        Ok((tenant_id, resolved_slug))
    }

    /// Deploy a draft tenant: create filesystem, container, health check, Caddy route.
    ///
    /// Requires tenant status='draft' and a valid config (provider + api_key set).
    pub async fn deploy_tenant(
        &self,
        tenant_id: &str,
        db: &DbPool,
        docker: &DockerManager,
        vault: &VaultService,
        proxy: Option<&ProxyManager>,
    ) -> Result<()> {
        // 1. Read tenant info, verify status='draft'
        let (slug, port, uid, plan, status): (String, u16, u32, String, String) =
            db.read(|conn| {
                conn.query_row(
                    "SELECT slug, port, uid, plan, status FROM tenants WHERE id = ?1",
                    rusqlite::params![tenant_id],
                    |row| {
                        Ok((
                            row.get(0)?,
                            row.get::<_, i64>(1).map(|p| p as u16)?,
                            row.get::<_, i64>(2).map(|u| u as u32)?,
                            row.get(3)?,
                            row.get(4)?,
                        ))
                    },
                )
                .map_err(|e| anyhow::anyhow!("tenant not found: {}", e))
            })?;

        if status != "draft" {
            bail!(
                "tenant is '{}', not 'draft' — only draft tenants can be deployed",
                status
            );
        }

        // 2. Read config, verify api_key is set
        let (api_key_enc, provider, model, _temp, autonomy, system_prompt): (
            String,
            String,
            String,
            f64,
            String,
            Option<String>,
        ) = db.read(|conn| {
            conn.query_row(
                "SELECT api_key_enc, provider, model, temperature, autonomy_level, system_prompt
                 FROM tenant_configs WHERE tenant_id = ?1",
                rusqlite::params![tenant_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .map_err(|e| anyhow::anyhow!("config not found: {}", e))
        })?;

        let api_key = vault.decrypt(&api_key_enc)?;
        if api_key == "__pending__" || provider.is_empty() {
            bail!("tenant config incomplete: set provider and API key before deploying");
        }

        // 3. Update status to provisioning
        db.write(|conn| {
            conn.execute(
                "UPDATE tenants SET status = 'provisioning' WHERE id = ?1",
                rusqlite::params![tenant_id],
            )?;
            Ok(())
        })?;

        // 4. Create filesystem dirs
        if let Err(e) = filesystem::create_tenant_dirs(&self.data_dir, &slug, uid) {
            tracing::error!("filesystem setup failed for {}: {}", slug, e);
            db.write(|conn| {
                conn.execute(
                    "UPDATE tenants SET status = 'error' WHERE id = ?1",
                    rusqlite::params![tenant_id],
                )?;
                Ok(())
            })?;
            bail!("filesystem setup failed: {}", e);
        }

        // 5. Read channels and write config.toml
        let channel_rows: Vec<(String, String)> = db.read(|conn| {
            let mut stmt = conn.prepare(
                "SELECT kind, config_enc FROM channels WHERE tenant_id = ?1 AND enabled = 1",
            )?;
            let rows = stmt
                .query_map(rusqlite::params![tenant_id], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })?;

        let mut channels: Vec<(String, serde_json::Value)> = Vec::new();
        for (kind, enc) in &channel_rows {
            if let Ok(json_str) = vault.decrypt(enc) {
                if let Ok(val) = serde_json::from_str(&json_str) {
                    channels.push((kind.clone(), val));
                }
            }
        }

        let mut config_content = config_render::render_config_toml(
            &autonomy,
            system_prompt.as_deref(),
            &self.egress_proxy_url,
        );
        if !channels.is_empty() {
            config_content.push('\n');
            config_content.push_str(&config_render::render_channel_config(&channels));
        }

        // Append tool settings from extra_json
        let extra_json: Option<String> = db
            .read(|conn| {
                conn.query_row(
                    "SELECT extra_json FROM tenant_configs WHERE tenant_id = ?1",
                    rusqlite::params![tenant_id],
                    |row| row.get(0),
                )
                .map_err(|e| anyhow::anyhow!("{}", e))
            })
            .unwrap_or(None);

        if let Some(ref ej) = extra_json {
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(ej) {
                if let Some(ts) = parsed.get("tool_settings") {
                    config_content.push_str(&config_render::render_tool_settings(ts, vault));
                }
            }
        }

        filesystem::write_tenant_config(&self.data_dir, &slug, &config_content, uid)?;

        // 6. Build env vars and create container
        let env_vars_owned = config_render::build_env_vars(&api_key, &provider, &model, port);
        let env_refs: Vec<(&str, &str)> = env_vars_owned
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();

        let (memory_mb, cpu_limit) = match plan.as_str() {
            "pro" => (512u32, 1.0f64),
            _ => (256u32, 0.5f64),
        };

        let container_id =
            match docker.create_container(&slug, port, uid, &env_refs, memory_mb, cpu_limit) {
                Ok(id) => id,
                Err(e) => {
                    tracing::error!("docker create failed for {}: {}", slug, e);
                    db.write(|conn| {
                        conn.execute(
                            "UPDATE tenants SET status = 'error' WHERE id = ?1",
                            rusqlite::params![tenant_id],
                        )?;
                        Ok(())
                    })?;
                    bail!("container creation failed: {}", e);
                }
            };

        db.write(|conn| {
            conn.execute(
                "UPDATE tenants SET container_id = ?1 WHERE id = ?2",
                rusqlite::params![container_id, tenant_id],
            )?;
            Ok(())
        })?;

        // 7. Health poll
        let healthy = health::poll_health(port, 30).await.unwrap_or(false);
        let final_status = if healthy { "running" } else { "error" };

        db.write(|conn| {
            conn.execute(
                "UPDATE tenants SET status = ?1 WHERE id = ?2",
                rusqlite::params![final_status, tenant_id],
            )?;
            Ok(())
        })?;

        if !healthy {
            tracing::warn!("deploy {} health check timed out; status=error", slug);
            let _ = docker.stop_container(&slug);
        }

        if healthy {
            if let Err(e) = pairing::read_and_store_pairing_code(docker, db, tenant_id, &slug).await {
                tracing::warn!("failed to read pairing code for {}: {}", slug, e);
            }
            if let Some(proxy) = proxy {
                if let Err(e) = proxy.add_route(&slug, port).await {
                    tracing::warn!("failed to add Caddy route for {}: {}", slug, e);
                }
            }
        }

        Ok(())
    }

    /// Delete a tenant: stop container, remove Caddy route, remove filesystem, remove DB.
    ///
    /// Order (M11): stop container → Caddy route removal → filesystem → DB records.
    /// Filesystem is removed before DB so orphaned files cannot exist after DB deletion.
    pub async fn delete_tenant(
        &self,
        tenant_id: &str,
        db: &DbPool,
        docker: &DockerManager,
        proxy: Option<&ProxyManager>,
    ) -> Result<()> {
        // 1. Fetch slug for filesystem and Caddy cleanup
        let slug: String = db.read(|conn| {
            conn.query_row(
                "SELECT slug FROM tenants WHERE id = ?1",
                rusqlite::params![tenant_id],
                |row| row.get(0),
            )
            .map_err(|e| anyhow::anyhow!("tenant not found: {}", e))
        })?;

        // 2. Stop and remove container (best-effort)
        let _ = docker.stop_container(&slug);
        let _ = docker.remove_container(&slug);

        // 3. Remove Caddy route (H6)
        if let Some(proxy) = proxy {
            if let Err(e) = proxy.remove_route(&slug).await {
                tracing::warn!("failed to remove Caddy route for {}: {}", slug, e);
            }
        }

        // 4. Remove filesystem FIRST (M11 — before DB so orphaned files can't exist)
        filesystem::remove_tenant_dirs(&self.data_dir, &slug)?;

        // 5. Remove DB records LAST
        db.write(|conn| {
            conn.execute(
                "DELETE FROM tenant_configs WHERE tenant_id = ?1",
                rusqlite::params![tenant_id],
            )?;
            conn.execute(
                "DELETE FROM tenants WHERE id = ?1",
                rusqlite::params![tenant_id],
            )?;
            Ok(())
        })?;

        Ok(())
    }

    /// Restart a tenant container with full config sync.
    ///
    /// Rewrites config.toml + recreates container with latest env vars.
    pub async fn restart_tenant(
        &self,
        tenant_id: &str,
        db: &DbPool,
        docker: &DockerManager,
        vault: &VaultService,
        proxy: Option<&ProxyManager>,
    ) -> Result<()> {
        self.sync_and_restart(tenant_id, db, docker, vault, proxy)
            .await
    }

    /// Sync tenant config + channels from DB into container.
    ///
    /// Flow:
    /// 1. Read tenant_configs (decrypt API key) + channels (decrypt configs) from DB
    /// 2. Rewrite config.toml with channel sections
    /// 3. Stop + remove old container
    /// 4. Create new container with updated env vars (provider, model, API key)
    /// 5. Health poll → update status
    /// 6. Auto-pair gateway
    ///
    /// Called after: config update, channel add/remove, restart.
    pub async fn sync_and_restart(
        &self,
        tenant_id: &str,
        db: &DbPool,
        docker: &DockerManager,
        vault: &VaultService,
        proxy: Option<&ProxyManager>,
    ) -> Result<()> {
        // 1. Read tenant info
        let (slug, port, uid, plan): (String, u16, u32, String) = db.read(|conn| {
            conn.query_row(
                "SELECT slug, port, uid, plan FROM tenants WHERE id = ?1",
                rusqlite::params![tenant_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get::<_, i64>(1).map(|p| p as u16)?,
                        row.get::<_, i64>(2).map(|u| u as u32)?,
                        row.get(3)?,
                    ))
                },
            )
            .map_err(|e| anyhow::anyhow!("tenant not found: {}", e))
        })?;

        // 2. Read config
        let (api_key_enc, provider, model, _temp, autonomy, system_prompt): (
            String,
            String,
            String,
            f64,
            String,
            Option<String>,
        ) = db.read(|conn| {
            conn.query_row(
                "SELECT api_key_enc, provider, model, temperature, autonomy_level, system_prompt
                 FROM tenant_configs WHERE tenant_id = ?1",
                rusqlite::params![tenant_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .map_err(|e| anyhow::anyhow!("config not found: {}", e))
        })?;

        // Decrypt API key
        let api_key = vault.decrypt(&api_key_enc)?;

        // 3. Read channels (decrypt configs)
        let channel_rows: Vec<(String, String)> = db.read(|conn| {
            let mut stmt = conn.prepare(
                "SELECT kind, config_enc FROM channels WHERE tenant_id = ?1 AND enabled = 1",
            )?;
            let rows = stmt
                .query_map(rusqlite::params![tenant_id], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })?;

        let mut channels: Vec<(String, serde_json::Value)> = Vec::new();
        for (kind, config_enc) in &channel_rows {
            match vault.decrypt(config_enc) {
                Ok(json_str) => match serde_json::from_str(&json_str) {
                    Ok(val) => channels.push((kind.clone(), val)),
                    Err(e) => {
                        tracing::warn!("skip channel {}: bad JSON: {}", kind, e);
                    }
                },
                Err(e) => {
                    tracing::warn!("skip channel {}: decrypt failed: {}", kind, e);
                }
            }
        }

        // 4. Preserve paired_tokens from existing config (before rewrite)
        let existing_paired_tokens = filesystem::read_paired_tokens(&self.data_dir, &slug);

        // 4a. Rewrite config.toml
        let mut config_content = config_render::render_config_toml(
            &autonomy,
            system_prompt.as_deref(),
            &self.egress_proxy_url,
        );

        // Re-inject preserved paired_tokens into [gateway] section
        if let Some(ref tokens_line) = existing_paired_tokens {
            config_content.push_str(tokens_line);
            config_content.push('\n');
        }

        if !channels.is_empty() {
            config_content.push('\n');
            config_content.push_str(&config_render::render_channel_config(&channels));
        }

        // Append tool settings from extra_json
        let extra_json: Option<String> = db
            .read(|conn| {
                conn.query_row(
                    "SELECT extra_json FROM tenant_configs WHERE tenant_id = ?1",
                    rusqlite::params![tenant_id],
                    |row| row.get(0),
                )
                .map_err(|e| anyhow::anyhow!("{}", e))
            })
            .unwrap_or(None);

        if let Some(ref ej) = extra_json {
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(ej) {
                if let Some(ts) = parsed.get("tool_settings") {
                    config_content.push_str(&config_render::render_tool_settings(ts, vault));
                }
            }
        }

        filesystem::write_tenant_config(&self.data_dir, &slug, &config_content, uid)?;

        // 4b. Ensure all tenant dirs/files are owned by container uid
        filesystem::ensure_tenant_ownership(&self.data_dir, &slug, uid)?;

        // 5. Stop + remove old container
        let _ = docker.stop_container(&slug);
        let _ = docker.remove_container(&slug);

        // 6. Create new container with updated env vars
        let env_vars_owned = config_render::build_env_vars(&api_key, &provider, &model, port);
        let env_refs: Vec<(&str, &str)> = env_vars_owned
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();

        let (memory_mb, cpu_limit) = match plan.as_str() {
            "pro" => (512u32, 1.0f64),
            _ => (256u32, 0.5f64),
        };

        let container_id =
            docker.create_container(&slug, port, uid, &env_refs, memory_mb, cpu_limit)?;

        db.write(|conn| {
            conn.execute(
                "UPDATE tenants SET container_id = ?1, status = 'starting' WHERE id = ?2",
                rusqlite::params![container_id, tenant_id],
            )?;
            Ok(())
        })?;

        // 7. Health poll
        let healthy = health::poll_health(port, 30).await.unwrap_or(false);
        let final_status = if healthy { "running" } else { "error" };

        db.write(|conn| {
            conn.execute(
                "UPDATE tenants SET status = ?1 WHERE id = ?2",
                rusqlite::params![final_status, tenant_id],
            )?;
            Ok(())
        })?;

        // 8. Handle pairing code
        if healthy {
            if existing_paired_tokens.is_some() {
                // User already paired — clear stale code from DB (no new code generated)
                let _ = pairing::clear_pairing_code(db, tenant_id);
            } else {
                // Fresh container, no paired tokens — read and store the new code
                if let Err(e) = pairing::read_and_store_pairing_code(docker, db, tenant_id, &slug).await
                {
                    tracing::warn!("failed to read pairing code for {}: {}", slug, e);
                }
            }
        }

        // 9. Sync Caddy route
        if healthy {
            if let Some(proxy) = proxy {
                if let Err(e) = proxy.add_route(&slug, port).await {
                    tracing::warn!("failed to update Caddy route for {}: {}", slug, e);
                }
            }
        }

        Ok(())
    }

    /// Roll back DB records after a failed provisioning attempt.
    fn rollback_db(&self, db: &DbPool, tenant_id: &str) -> Result<()> {
        let _ = db.write(|conn| {
            conn.execute(
                "DELETE FROM tenant_configs WHERE tenant_id = ?1",
                rusqlite::params![tenant_id],
            )?;
            conn.execute(
                "DELETE FROM tenants WHERE id = ?1",
                rusqlite::params![tenant_id],
            )?;
            Ok(())
        });
        Ok(())
    }

    /// Roll back filesystem dirs after a failed provisioning attempt.
    fn rollback_filesystem(&self, slug: &str) -> Result<()> {
        let _ = filesystem::remove_tenant_dirs(&self.data_dir, slug);
        Ok(())
    }
}
