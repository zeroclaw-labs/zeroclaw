pub mod caddy;

use anyhow::{bail, Result};

pub struct ProxyManager {
    caddy_api_url: String,
    domain: String,
    client: reqwest::Client,
}

impl ProxyManager {
    pub fn new(caddy_api_url: &str, domain: &str) -> Self {
        Self {
            caddy_api_url: caddy_api_url.to_string(),
            domain: domain.to_string(),
            client: reqwest::Client::new(),
        }
    }

    /// Add a route for a tenant: {slug}.{domain} -> 127.0.0.1:{port}
    pub async fn add_route(&self, slug: &str, port: u16) -> Result<()> {
        let hostname = format!("{}.{}", slug, self.domain);
        let route = caddy::build_route(&hostname, port);
        let url = format!(
            "{}/config/apps/http/servers/srv0/routes",
            self.caddy_api_url
        );
        let resp = self.client.post(&url).json(&route).send().await?;
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            bail!("Caddy add route failed: {}", body);
        }
        tracing::info!("Caddy route added: {} -> 127.0.0.1:{}", hostname, port);
        Ok(())
    }

    /// Remove a route for a tenant by finding its index in the routes array.
    pub async fn remove_route(&self, slug: &str) -> Result<()> {
        let hostname = format!("{}.{}", slug, self.domain);

        // Fetch current routes list
        let list_url = format!(
            "{}/config/apps/http/servers/srv0/routes",
            self.caddy_api_url
        );
        let resp = self.client.get(&list_url).send().await?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            bail!("Caddy list routes failed: {}", body);
        }

        let routes: serde_json::Value = resp.json().await?;
        let routes_arr = routes
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("unexpected routes response format"))?;

        // Find the index of the route matching this hostname
        let index = routes_arr.iter().position(|r| {
            r["match"]
                .as_array()
                .and_then(|m| m.first())
                .and_then(|m| m["host"].as_array())
                .and_then(|h| h.first())
                .and_then(|h| h.as_str())
                .map(|h| h == hostname)
                .unwrap_or(false)
        });

        let Some(idx) = index else {
            tracing::warn!("Caddy route not found for {}, skipping remove", hostname);
            return Ok(());
        };

        let delete_url = format!(
            "{}/config/apps/http/servers/srv0/routes/{}",
            self.caddy_api_url, idx
        );
        let resp = self.client.delete(&delete_url).send().await?;
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            bail!("Caddy remove route failed: {}", body);
        }
        tracing::info!("Caddy route removed: {}", hostname);
        Ok(())
    }

    /// Load initial Caddy config with wildcard TLS and platform API route.
    pub async fn init_config(&self, platform_port: u16) -> Result<()> {
        let config = caddy::build_initial_config(&self.domain, platform_port);
        let url = format!("{}/load", self.caddy_api_url);
        let resp = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .json(&config)
            .send()
            .await?;
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            bail!("Caddy init config failed: {}", body);
        }
        tracing::info!(
            "Caddy initial config loaded for domain {} (platform port {})",
            self.domain,
            platform_port
        );
        Ok(())
    }

    /// Sync all tenant routes from DB to Caddy (used on startup).
    /// `tenants` is a list of (slug, port) pairs.
    pub async fn sync_routes(&self, tenants: &[(String, u16)]) -> Result<()> {
        for (slug, port) in tenants {
            if let Err(e) = self.add_route(slug, *port).await {
                tracing::warn!("Failed to sync route for {}: {}", slug, e);
            }
        }
        tracing::info!("Caddy route sync complete ({} tenants)", tenants.len());
        Ok(())
    }
}
