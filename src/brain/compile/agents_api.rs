//! Read-only Paperclip API client. Fetches companies and agents over loopback
//! HTTP. The local Paperclip server treats loopback as `local_implicit` board
//! auth, so no token is needed.

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Company {
    pub id: String,
    #[serde(default)]
    pub status: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Agent {
    pub id: String,
    pub company_id: String,
    pub name: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub capabilities: Option<String>,
    #[serde(default)]
    pub reports_to: Option<String>,
}

pub async fn list_companies(host: &str) -> Result<Vec<Company>> {
    let url = format!("{host}/api/companies");
    let res = reqwest::get(&url)
        .await
        .with_context(|| format!("GET {url}"))?
        .error_for_status()?;
    let companies: Vec<Company> = res.json().await.context("parse companies json")?;
    Ok(companies)
}

pub async fn list_agents(host: &str, company_id: &str) -> Result<Vec<Agent>> {
    let url = format!("{host}/api/companies/{company_id}/agents");
    let res = reqwest::get(&url)
        .await
        .with_context(|| format!("GET {url}"))?
        .error_for_status()?;
    let agents: Vec<Agent> = res.json().await.context("parse agents json")?;
    Ok(agents)
}
