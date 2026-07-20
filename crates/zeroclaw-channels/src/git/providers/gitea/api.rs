//! Minimal Gitea-compatible REST client.

use chrono::{DateTime, Utc};
use serde::de::DeserializeOwned;

use super::payloads::{
    CreatedComment, GITEA_USER_AGENT, GiteaComment, GiteaIssue, GiteaRelease, GiteaRepo, GiteaUser,
};
use crate::git::types::{GitChannelError, IssueRef, RepoRef};

/// Pin a raw forge-call path under the configured Gitea API base. Same origin
/// guarantee as the GitHub client: the path is only ever appended after
/// `base/`, so no absolute URL, `//host`, or `..` in the path can redirect the
/// request to a different origin.
fn forge_url(base: &str, path: &str) -> String {
    format!("{}/{}", base, path.trim_start_matches('/'))
}

pub struct GiteaApi {
    base: String,
    proxy_url: Option<String>,
}

impl GiteaApi {
    /// `api_base_url` must be non-blank - callers (`build_provider`, tests)
    /// resolve and validate it first. Deliberately no fallback host here:
    /// every request carries the operator's access token.
    pub fn new(api_base_url: String, proxy_url: Option<String>) -> Self {
        let base = api_base_url.trim().trim_end_matches('/').to_string();
        Self { base, proxy_url }
    }

    fn http_client(&self) -> reqwest::Client {
        zeroclaw_config::schema::build_channel_proxy_client_with_timeouts(
            "channel.git.gitea",
            self.proxy_url.as_deref(),
            30,
            10,
        )
    }

    fn request(
        &self,
        method: reqwest::Method,
        url: String,
        token: &str,
    ) -> reqwest::RequestBuilder {
        self.http_client()
            .request(method, url)
            .bearer_auth(token)
            .header(reqwest::header::ACCEPT, "application/json")
            .header(reqwest::header::USER_AGENT, GITEA_USER_AGENT)
    }

    async fn execute<T: DeserializeOwned>(
        &self,
        endpoint: &str,
        req: reqwest::RequestBuilder,
    ) -> Result<T, GitChannelError> {
        let resp = req.send().await?;
        if resp.status().is_success() {
            return Ok(resp.json().await?);
        }
        Err(Self::error_from(endpoint, resp).await)
    }

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
        if status.as_u16() == 429 {
            let retry_after = resp
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<i64>().ok())
                .unwrap_or(60);
            return GitChannelError::RateLimited {
                reset_at: Utc::now() + chrono::Duration::seconds(retry_after),
            };
        }
        let body = resp.text().await.unwrap_or_default();
        GitChannelError::Api {
            endpoint: endpoint.to_string(),
            status: status.as_u16(),
            body: zeroclaw_providers::sanitize_api_error(&body),
        }
    }

    async fn get_paged<T: DeserializeOwned>(
        &self,
        endpoint: &str,
        token: &str,
        base_url: &str,
        per_page: usize,
    ) -> Result<Vec<T>, GitChannelError> {
        let sep = if base_url.contains('?') { '&' } else { '?' };
        let mut out: Vec<T> = Vec::new();
        for page in 1..=1000u32 {
            let url = format!("{base_url}{sep}limit={per_page}&page={page}");
            let batch: Vec<T> = self
                .execute(endpoint, self.request(reqwest::Method::GET, url, token))
                .await?;
            let n = batch.len();
            out.extend(batch);
            if n < per_page {
                break;
            }
        }
        Ok(out)
    }

    pub async fn current_user(&self, token: &str) -> Result<GiteaUser, GitChannelError> {
        let url = format!("{}/user", self.base);
        self.execute("GET /user", self.request(reqwest::Method::GET, url, token))
            .await
    }

    pub async fn list_user_repos(&self, token: &str) -> Result<Vec<RepoRef>, GitChannelError> {
        let base = format!("{}/user/repos", self.base);
        let repos: Vec<GiteaRepo> = self.get_paged("GET /user/repos", token, &base, 100).await?;
        Ok(repos
            .iter()
            .filter_map(|r| RepoRef::parse(&r.full_name))
            .collect())
    }

    pub async fn list_issues_since(
        &self,
        token: &str,
        repo: &RepoRef,
        since: DateTime<Utc>,
    ) -> Result<Vec<GiteaIssue>, GitChannelError> {
        let base = format!(
            "{}/repos/{repo}/issues?state=all&since={}",
            self.base,
            since.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        );
        self.get_paged("GET /repos/{owner}/{repo}/issues", token, &base, 100)
            .await
    }

    pub async fn list_issue_comments_since(
        &self,
        token: &str,
        repo: &RepoRef,
        since: DateTime<Utc>,
    ) -> Result<Vec<GiteaComment>, GitChannelError> {
        let base = format!(
            "{}/repos/{repo}/issues/comments?since={}",
            self.base,
            since.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        );
        self.get_paged(
            "GET /repos/{owner}/{repo}/issues/comments",
            token,
            &base,
            100,
        )
        .await
    }

    pub async fn list_releases(
        &self,
        token: &str,
        repo: &RepoRef,
    ) -> Result<Vec<GiteaRelease>, GitChannelError> {
        let base = format!("{}/repos/{repo}/releases", self.base);
        self.get_paged("GET /repos/{owner}/{repo}/releases", token, &base, 30)
            .await
    }

    pub async fn create_comment(
        &self,
        token: &str,
        issue: &IssueRef,
        body: &str,
    ) -> Result<u64, GitChannelError> {
        let url = format!(
            "{}/repos/{}/issues/{}/comments",
            self.base, issue.repo, issue.number
        );
        let created: CreatedComment = self
            .execute(
                "POST /repos/{owner}/{repo}/issues/{index}/comments",
                self.request(reqwest::Method::POST, url, token)
                    .json(&serde_json::json!({ "body": body })),
            )
            .await?;
        Ok(created.id)
    }

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
            "POST /repos/{owner}/{repo}/issues/{index}/reactions",
            self.request(reqwest::Method::POST, url, token)
                .json(&serde_json::json!({ "content": content })),
        )
        .await
    }

    /// Low-level authed call: build `{base}/{path}`, attach `token`, send, and
    /// return the raw status + decoded JSON body (Null when empty). Non-2xx is
    /// returned, not raised, so the caller inspects Gitea's error envelope.
    pub async fn forge_call(
        &self,
        token: &str,
        method: reqwest::Method,
        path: &str,
        body: Option<&serde_json::Value>,
    ) -> Result<(u16, serde_json::Value), GitChannelError> {
        let url = forge_url(&self.base, path);
        let mut req = self.request(method, url, token);
        if let Some(payload) = body {
            req = req.json(payload);
        }
        let resp = req.send().await?;
        let status = resp.status().as_u16();
        let text = resp.text().await?;
        let value = if text.trim().is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or(serde_json::Value::String(text))
        };
        Ok((status, value))
    }
}

#[cfg(test)]
mod tests {
    use super::forge_url;

    #[test]
    fn forge_url_pins_raw_path_to_configured_base() {
        let base = "https://git.example.org/api/v1";
        assert_eq!(
            forge_url(base, "repos/o/r/issues/1"),
            "https://git.example.org/api/v1/repos/o/r/issues/1"
        );
        assert_eq!(
            forge_url(base, "/repos/o/r/issues/1"),
            "https://git.example.org/api/v1/repos/o/r/issues/1"
        );
        assert!(forge_url(base, "https://evil.test/x").starts_with("https://git.example.org/"));
        assert!(forge_url(base, "//evil.test/x").starts_with("https://git.example.org/"));
        assert!(forge_url(base, "../../../evil.test").starts_with("https://git.example.org/"));
    }
}
