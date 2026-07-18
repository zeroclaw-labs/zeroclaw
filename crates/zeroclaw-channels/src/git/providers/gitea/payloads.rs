//! Gitea-compatible REST payloads.

use chrono::{DateTime, Utc};
use serde::Deserialize;

pub const GITEA_USER_AGENT: &str = "zeroclaw";

#[derive(Debug, Clone, Deserialize)]
pub struct GiteaUser {
    #[serde(default)]
    pub login: String,
    #[serde(default)]
    pub username: String,
    #[serde(default, rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub is_bot: bool,
}

impl GiteaUser {
    pub fn login(&self) -> String {
        if self.login.is_empty() {
            self.username.clone()
        } else {
            self.login.clone()
        }
    }

    pub fn is_bot(&self) -> bool {
        self.is_bot || self.kind.eq_ignore_ascii_case("bot") || self.login().ends_with("[bot]")
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct GiteaRepo {
    pub full_name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GiteaIssue {
    pub id: u64,
    #[serde(alias = "number")]
    pub index: u64,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub body: String,
    pub user: GiteaUser,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub closed_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub html_url: String,
    #[serde(default)]
    pub pull_request: Option<GiteaPullStub>,
}

impl GiteaIssue {
    pub fn is_pull_request(&self) -> bool {
        self.pull_request.is_some()
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct GiteaPullStub {
    #[serde(default)]
    pub merged: bool,
    #[serde(default)]
    pub merged_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GiteaComment {
    pub id: u64,
    #[serde(default)]
    pub body: String,
    pub user: GiteaUser,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub issue_url: String,
}

impl GiteaComment {
    pub fn issue_number(&self) -> Option<u64> {
        self.issue_url.rsplit('/').next()?.parse().ok()
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct GiteaRelease {
    pub id: u64,
    pub tag_name: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub body: String,
    pub author: GiteaUser,
    #[serde(default)]
    pub draft: bool,
    #[serde(default, alias = "created_at")]
    pub published_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub html_url: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreatedComment {
    pub id: u64,
}
