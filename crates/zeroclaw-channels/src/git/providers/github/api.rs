//! Minimal GitHub REST client for the provider.
//!
//! Typed wrappers over the handful of endpoints the channel uses. Every
//! method takes its auth credential (app JWT or installation token) as
//! an argument; this module knows nothing about how credentials are
//! minted, and nothing about polling or message mapping.

use chrono::{DateTime, Utc};
use serde::de::DeserializeOwned;

use super::payloads::{
    CachedToken, GITHUB_ACCEPT, GITHUB_API_BASE, GITHUB_API_VERSION, GITHUB_USER_AGENT, GhApp,
    GhComment, GhInstallation, GhIssue, GhRelease, GhRepo, GhReviewComment, GhTokenResponse,
    GhWorkflowRun, InstallationId, RepoEventsPage,
};
use crate::git::types::{GitChannelError, IssueRef, RepoRef};

pub struct GithubApi {
    base: String,
    proxy_url: Option<String>,
}

impl GithubApi {
    pub fn new(proxy_url: Option<String>) -> Self {
        Self {
            base: GITHUB_API_BASE.to_string(),
            proxy_url,
        }
    }

    /// Point the client at a mock server instead of api.github.com.
    #[cfg(test)]
    pub(crate) fn with_base(base: String) -> Self {
        Self {
            base,
            proxy_url: None,
        }
    }

    fn http_client(&self) -> reqwest::Client {
        zeroclaw_config::schema::build_channel_proxy_client_with_timeouts(
            "channel.git.github",
            self.proxy_url.as_deref(),
            30,
            10,
        )
    }

    fn request(
        &self,
        method: reqwest::Method,
        url: String,
        auth_header: &str,
    ) -> reqwest::RequestBuilder {
        self.http_client()
            .request(method, url)
            .bearer_auth(auth_header)
            .header(reqwest::header::ACCEPT, GITHUB_ACCEPT)
            .header(reqwest::header::USER_AGENT, GITHUB_USER_AGENT)
            .header("X-GitHub-Api-Version", GITHUB_API_VERSION)
    }

    /// Send a request and decode a JSON body, mapping GitHub's rate-limit
    /// and error envelopes onto typed errors.
    async fn execute<T: DeserializeOwned>(
        &self,
        endpoint: &str,
        req: reqwest::RequestBuilder,
    ) -> Result<T, GitChannelError> {
        let resp = req.send().await?;
        let status = resp.status();
        if status.is_success() {
            return Ok(resp.json().await?);
        }
        Err(Self::error_from(endpoint, resp).await)
    }

    /// Variant of [`Self::execute`] for endpoints whose success body is
    /// irrelevant (204s, reaction envelopes).
    async fn execute_unit(
        &self,
        endpoint: &str,
        req: reqwest::RequestBuilder,
    ) -> Result<(), GitChannelError> {
        let resp = req.send().await?;
        if resp.status().is_success() {
            return Ok(());
        }
        Err(Self::error_from(endpoint, resp).await)
    }

    async fn error_from(endpoint: &str, resp: reqwest::Response) -> GitChannelError {
        let status = resp.status();
        if status.as_u16() == 429 || status.as_u16() == 403 {
            let remaining = resp
                .headers()
                .get("x-ratelimit-remaining")
                .and_then(|v| v.to_str().ok());
            let retry_after = resp
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<i64>().ok());
            let reset = resp
                .headers()
                .get("x-ratelimit-reset")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<i64>().ok());
            if remaining == Some("0") || retry_after.is_some() {
                let reset_at = reset
                    .and_then(|epoch| DateTime::<Utc>::from_timestamp(epoch, 0))
                    .unwrap_or_else(|| {
                        Utc::now() + chrono::Duration::seconds(retry_after.unwrap_or(60))
                    });
                return GitChannelError::RateLimited { reset_at };
            }
        }
        let body = resp.text().await.unwrap_or_default();
        GitChannelError::Api {
            endpoint: endpoint.to_string(),
            status: status.as_u16(),
            body: zeroclaw_providers::sanitize_api_error(&body),
        }
    }

    // ── App-scoped endpoints (authenticate with the app JWT) ───────

    /// `GET /app` — the app's slug (login is `<slug>[bot]`).
    pub async fn app_slug(&self, jwt: &str) -> Result<String, GitChannelError> {
        let url = format!("{}/app", self.base);
        let app: GhApp = self
            .execute("GET /app", self.request(reqwest::Method::GET, url, jwt))
            .await?;
        Ok(app.slug)
    }

    /// `GET /app/installations`.
    pub async fn list_installations(
        &self,
        jwt: &str,
    ) -> Result<Vec<GhInstallation>, GitChannelError> {
        let url = format!("{}/app/installations?per_page=100", self.base);
        self.execute(
            "GET /app/installations",
            self.request(reqwest::Method::GET, url, jwt),
        )
        .await
    }

    /// `POST /app/installations/{id}/access_tokens` — exchange the app JWT
    /// for a ~1h installation token.
    pub async fn create_installation_token(
        &self,
        jwt: &str,
        installation: InstallationId,
    ) -> Result<CachedToken, GitChannelError> {
        let url = format!(
            "{}/app/installations/{installation}/access_tokens",
            self.base
        );
        let resp: GhTokenResponse = self
            .execute(
                "POST /app/installations/{id}/access_tokens",
                self.request(reqwest::Method::POST, url, jwt),
            )
            .await?;
        Ok(CachedToken {
            token: resp.token,
            expires_at: resp.expires_at,
        })
    }

    // ── Installation-scoped endpoints (authenticate with the token) ──

    /// `GET /installation/repositories` — repos visible to the installation.
    pub async fn list_installation_repos(
        &self,
        token: &str,
    ) -> Result<Vec<RepoRef>, GitChannelError> {
        #[derive(serde::Deserialize)]
        struct Page {
            total_count: u64,
            repositories: Vec<GhRepo>,
        }
        let url = format!("{}/installation/repositories?per_page=100", self.base);
        let page: Page = self
            .execute(
                "GET /installation/repositories",
                self.request(reqwest::Method::GET, url, token),
            )
            .await?;
        if page.total_count as usize > page.repositories.len() {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({
                        "total": page.total_count,
                        "polled": page.repositories.len(),
                    })),
                "installation has more than 100 repositories; only the first page is polled — \
                 set `repos` explicitly to choose"
            );
        }
        Ok(page
            .repositories
            .iter()
            .filter_map(|r| RepoRef::parse(&r.full_name))
            .collect())
    }

    /// `GET /repos/{owner}/{repo}/issues/comments?since=…` — comments on
    /// issues and PRs, oldest first.
    pub async fn list_issue_comments_since(
        &self,
        token: &str,
        repo: &RepoRef,
        since: DateTime<Utc>,
    ) -> Result<Vec<GhComment>, GitChannelError> {
        let url = format!(
            "{}/repos/{repo}/issues/comments?since={}&sort=created&direction=asc&per_page=100",
            self.base,
            since.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        );
        self.execute(
            "GET /repos/{owner}/{repo}/issues/comments",
            self.request(reqwest::Method::GET, url, token),
        )
        .await
    }

    /// `GET /repos/{owner}/{repo}/issues?since=…` — issues and PRs
    /// (opening posts), oldest first. `since` filters on `updated_at`;
    /// callers filter on `created_at` to ignore edits.
    pub async fn list_issues_since(
        &self,
        token: &str,
        repo: &RepoRef,
        since: DateTime<Utc>,
    ) -> Result<Vec<GhIssue>, GitChannelError> {
        let url = format!(
            "{}/repos/{repo}/issues?since={}&state=all&sort=created&direction=asc&per_page=100",
            self.base,
            since.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        );
        self.execute(
            "GET /repos/{owner}/{repo}/issues",
            self.request(reqwest::Method::GET, url, token),
        )
        .await
    }

    /// `GET /repos/{owner}/{repo}/pulls/comments?since=…` — inline PR
    /// review comments, oldest first.
    pub async fn list_review_comments_since(
        &self,
        token: &str,
        repo: &RepoRef,
        since: DateTime<Utc>,
    ) -> Result<Vec<GhReviewComment>, GitChannelError> {
        let url = format!(
            "{}/repos/{repo}/pulls/comments?since={}&sort=created&direction=asc&per_page=100",
            self.base,
            since.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        );
        self.execute(
            "GET /repos/{owner}/{repo}/pulls/comments",
            self.request(reqwest::Method::GET, url, token),
        )
        .await
    }

    /// `GET /repos/{owner}/{repo}/releases` — newest first. The endpoint
    /// has no `since` parameter; callers filter on `published_at`.
    pub async fn list_releases(
        &self,
        token: &str,
        repo: &RepoRef,
    ) -> Result<Vec<GhRelease>, GitChannelError> {
        let url = format!("{}/repos/{repo}/releases?per_page=30", self.base);
        self.execute(
            "GET /repos/{owner}/{repo}/releases",
            self.request(reqwest::Method::GET, url, token),
        )
        .await
    }

    /// `GET /repos/{owner}/{repo}/actions/runs?created=>=…` — workflow
    /// runs created at or after the cursor, any status. The Events API
    /// carries no Actions events, so this endpoint is the only source
    /// for CI outcomes.
    pub async fn list_workflow_runs_created_since(
        &self,
        token: &str,
        repo: &RepoRef,
        since: DateTime<Utc>,
    ) -> Result<Vec<GhWorkflowRun>, GitChannelError> {
        #[derive(serde::Deserialize)]
        struct Page {
            workflow_runs: Vec<GhWorkflowRun>,
        }
        let url = format!(
            "{}/repos/{repo}/actions/runs?created=%3E%3D{}&per_page=100",
            self.base,
            since.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        );
        let page: Page = self
            .execute(
                "GET /repos/{owner}/{repo}/actions/runs",
                self.request(reqwest::Method::GET, url, token),
            )
            .await?;
        Ok(page.workflow_runs)
    }

    /// `GET /repos/{owner}/{repo}/events` — the repository activity feed
    /// (Tier C backbone), polled conditionally: when `etag` is supplied
    /// and the feed is unchanged, GitHub answers 304 and the page comes
    /// back `not_modified` (free against the rate budget).
    pub async fn list_repo_events(
        &self,
        token: &str,
        repo: &RepoRef,
        etag: Option<&str>,
    ) -> Result<RepoEventsPage, GitChannelError> {
        let url = format!("{}/repos/{repo}/events?per_page=100", self.base);
        let mut req = self.request(reqwest::Method::GET, url, token);
        if let Some(etag) = etag {
            req = req.header(reqwest::header::IF_NONE_MATCH, etag);
        }
        let resp = req.send().await?;
        if resp.status() == reqwest::StatusCode::NOT_MODIFIED {
            return Ok(RepoEventsPage {
                events: Vec::new(),
                etag: None,
                not_modified: true,
            });
        }
        if !resp.status().is_success() {
            return Err(Self::error_from("GET /repos/{owner}/{repo}/events", resp).await);
        }
        let etag = resp
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        Ok(RepoEventsPage {
            events: resp.json().await?,
            etag,
            not_modified: false,
        })
    }

    /// `POST /repos/{owner}/{repo}/issues/{n}/comments` — returns the new
    /// comment's id.
    pub async fn create_comment(
        &self,
        token: &str,
        issue: &IssueRef,
        body: &str,
    ) -> Result<u64, GitChannelError> {
        #[derive(serde::Deserialize)]
        struct Created {
            id: u64,
        }
        let url = format!(
            "{}/repos/{}/issues/{}/comments",
            self.base, issue.repo, issue.number
        );
        let created: Created = self
            .execute(
                "POST /repos/{owner}/{repo}/issues/{n}/comments",
                self.request(reqwest::Method::POST, url, token)
                    .json(&serde_json::json!({ "body": body })),
            )
            .await?;
        Ok(created.id)
    }

    /// `PATCH /repos/{owner}/{repo}/issues/comments/{id}` — edit a comment.
    pub async fn update_comment(
        &self,
        token: &str,
        repo: &RepoRef,
        comment_id: u64,
        body: &str,
    ) -> Result<(), GitChannelError> {
        let url = format!("{}/repos/{repo}/issues/comments/{comment_id}", self.base);
        self.execute_unit(
            "PATCH /repos/{owner}/{repo}/issues/comments/{id}",
            self.request(reqwest::Method::PATCH, url, token)
                .json(&serde_json::json!({ "body": body })),
        )
        .await
    }

    /// `DELETE /repos/{owner}/{repo}/issues/comments/{id}`.
    pub async fn delete_comment(
        &self,
        token: &str,
        repo: &RepoRef,
        comment_id: u64,
    ) -> Result<(), GitChannelError> {
        let url = format!("{}/repos/{repo}/issues/comments/{comment_id}", self.base);
        self.execute_unit(
            "DELETE /repos/{owner}/{repo}/issues/comments/{id}",
            self.request(reqwest::Method::DELETE, url, token),
        )
        .await
    }

    /// `POST /repos/{owner}/{repo}/issues/comments/{id}/reactions`.
    pub async fn add_comment_reaction(
        &self,
        token: &str,
        repo: &RepoRef,
        comment_id: u64,
        content: &str,
    ) -> Result<(), GitChannelError> {
        let url = format!(
            "{}/repos/{repo}/issues/comments/{comment_id}/reactions",
            self.base
        );
        self.execute_unit(
            "POST /repos/{owner}/{repo}/issues/comments/{id}/reactions",
            self.request(reqwest::Method::POST, url, token)
                .json(&serde_json::json!({ "content": content })),
        )
        .await
    }

    /// `POST /repos/{owner}/{repo}/issues/{n}/reactions` — react to the
    /// issue/PR body itself.
    pub async fn add_issue_reaction(
        &self,
        token: &str,
        issue: &IssueRef,
        content: &str,
    ) -> Result<(), GitChannelError> {
        let url = format!(
            "{}/repos/{}/issues/{}/reactions",
            self.base, issue.repo, issue.number
        );
        self.execute_unit(
            "POST /repos/{owner}/{repo}/issues/{n}/reactions",
            self.request(reqwest::Method::POST, url, token)
                .json(&serde_json::json!({ "content": content })),
        )
        .await
    }
}
