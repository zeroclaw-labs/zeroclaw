use super::traits::{Channel, ChannelMessage, SendMessage};
use async_trait::async_trait;
use hmac::{Hmac, Mac};
use reqwest::{header::HeaderMap, StatusCode};
use sha2::Sha256;
use std::time::Duration;
use uuid::Uuid;

const DEFAULT_GITHUB_API_BASE: &str = "https://api.github.com";
const GITHUB_API_VERSION: &str = "2022-11-28";

/// GitHub channel in webhook mode.
///
/// Incoming events are received by the gateway endpoint `/github`.
/// Outbound replies are posted as issue/PR comments via GitHub REST API.
pub struct GitHubChannel {
    access_token: String,
    api_base_url: String,
    allowed_repos: Vec<String>,
    client: reqwest::Client,
}

impl GitHubChannel {
    pub fn new(
        access_token: String,
        api_base_url: Option<String>,
        allowed_repos: Vec<String>,
    ) -> Self {
        let base = api_base_url
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .unwrap_or(DEFAULT_GITHUB_API_BASE);
        Self {
            access_token,
            api_base_url: base.trim_end_matches('/').to_string(),
            allowed_repos,
            client: reqwest::Client::new(),
        }
    }

    fn now_unix_secs() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }

    fn parse_rfc3339_timestamp(raw: Option<&str>) -> u64 {
        raw.and_then(|value| {
            chrono::DateTime::parse_from_rfc3339(value)
                .ok()
                .map(|dt| dt.timestamp().max(0) as u64)
        })
        .unwrap_or_else(Self::now_unix_secs)
    }

    fn repo_is_allowed(&self, repo_full_name: &str) -> bool {
        if self.allowed_repos.is_empty() {
            return false;
        }

        self.allowed_repos.iter().any(|raw| {
            let allowed = raw.trim();
            if allowed.is_empty() {
                return false;
            }
            if allowed == "*" {
                return true;
            }
            if let Some(owner_prefix) = allowed.strip_suffix("/*") {
                if let Some((repo_owner, _)) = repo_full_name.split_once('/') {
                    return repo_owner.eq_ignore_ascii_case(owner_prefix);
                }
            }
            repo_full_name.eq_ignore_ascii_case(allowed)
        })
    }

    fn parse_issue_recipient(recipient: &str) -> Option<(&str, u64)> {
        let (repo, issue_no) = recipient.trim().rsplit_once('#')?;
        if !repo.contains('/') {
            return None;
        }
        let number = issue_no.parse::<u64>().ok()?;
        if number == 0 {
            return None;
        }
        Some((repo, number))
    }

    fn issue_comment_api_url(&self, repo_full_name: &str, issue_number: u64) -> Option<String> {
        let (owner, repo) = repo_full_name.split_once('/')?;
        let owner = urlencoding::encode(owner.trim());
        let repo = urlencoding::encode(repo.trim());
        Some(format!(
            "{}/repos/{owner}/{repo}/issues/{issue_number}/comments",
            self.api_base_url
        ))
    }

    fn is_rate_limited(status: StatusCode, headers: &HeaderMap) -> bool {
        if status == StatusCode::TOO_MANY_REQUESTS {
            return true;
        }
        status == StatusCode::FORBIDDEN
            && headers
                .get("x-ratelimit-remaining")
                .and_then(|v| v.to_str().ok())
                .map(str::trim)
                .is_some_and(|v| v == "0")
    }

    fn retry_delay_from_headers(headers: &HeaderMap) -> Option<Duration> {
        if let Some(raw) = headers.get("retry-after").and_then(|v| v.to_str().ok()) {
            if let Ok(secs) = raw.trim().parse::<u64>() {
                return Some(Duration::from_secs(secs.max(1).min(60)));
            }
        }

        let remaining_is_zero = headers
            .get("x-ratelimit-remaining")
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .is_some_and(|v| v == "0");
        if !remaining_is_zero {
            return None;
        }

        let reset = headers
            .get("x-ratelimit-reset")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok())?;
        let now = Self::now_unix_secs();
        let wait = if reset > now { reset - now } else { 1 };
        Some(Duration::from_secs(wait.max(1).min(60)))
    }

    async fn post_issue_comment(
        &self,
        repo_full_name: &str,
        issue_number: u64,
        body: &str,
    ) -> anyhow::Result<()> {
        let Some(url) = self.issue_comment_api_url(repo_full_name, issue_number) else {
            anyhow::bail!("invalid GitHub recipient repo format: {repo_full_name}");
        };

        let payload = serde_json::json!({ "body": body });
        let mut backoff = Duration::from_secs(1);

        for attempt in 1..=3 {
            let response = self
                .client
                .post(&url)
                .bearer_auth(&self.access_token)
                .header("Accept", "application/vnd.github+json")
                .header("X-GitHub-Api-Version", GITHUB_API_VERSION)
                .header("User-Agent", "ZeroClaw-GitHub-Channel")
                .json(&payload)
                .send()
                .await?;

            if response.status().is_success() {
                return Ok(());
            }

            let status = response.status();
            let headers = response.headers().clone();
            let body_text = response.text().await.unwrap_or_default();
            let sanitized = crate::providers::sanitize_api_error(&body_text);

            if attempt < 3 && Self::is_rate_limited(status, &headers) {
                let wait = Self::retry_delay_from_headers(&headers).unwrap_or(backoff);
                tracing::warn!(
                    "GitHub send rate-limited (status {status}), retrying in {}s (attempt {attempt}/3)",
                    wait.as_secs()
                );
                tokio::time::sleep(wait).await;
                backoff = (backoff * 2).min(Duration::from_secs(8));
                continue;
            }

            tracing::error!("GitHub comment post failed: {status} — {sanitized}");
            anyhow::bail!("GitHub API error: {status}");
        }

        anyhow::bail!("GitHub send retries exhausted")
    }

    fn is_bot_actor(login: Option<&str>, actor_type: Option<&str>) -> bool {
        actor_type
            .map(|v| v.eq_ignore_ascii_case("bot"))
            .unwrap_or(false)
            || login
                .map(|v| v.trim_end().ends_with("[bot]"))
                .unwrap_or(false)
    }

    fn parse_issue_comment_event(
        &self,
        payload: &serde_json::Value,
        event_name: &str,
    ) -> Vec<ChannelMessage> {
        let mut out = Vec::new();
        let action = payload
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        if action != "created" {
            return out;
        }

        let repo = payload
            .get("repository")
            .and_then(|v| v.get("full_name"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty());
        let Some(repo) = repo else {
            return out;
        };

        if !self.repo_is_allowed(repo) {
            tracing::warn!(
                "GitHub: ignoring webhook for unauthorized repository '{repo}'. \
                 Add repo to channels_config.github.allowed_repos or use '*' explicitly."
            );
            return out;
        }

        let comment = payload.get("comment");
        let comment_body = comment
            .and_then(|v| v.get("body"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty());
        let Some(comment_body) = comment_body else {
            return out;
        };

        let actor_login = comment
            .and_then(|v| v.get("user"))
            .and_then(|v| v.get("login"))
            .and_then(|v| v.as_str())
            .or_else(|| {
                payload
                    .get("sender")
                    .and_then(|v| v.get("login"))
                    .and_then(|v| v.as_str())
            });
        let actor_type = comment
            .and_then(|v| v.get("user"))
            .and_then(|v| v.get("type"))
            .and_then(|v| v.as_str())
            .or_else(|| {
                payload
                    .get("sender")
                    .and_then(|v| v.get("type"))
                    .and_then(|v| v.as_str())
            });

        if Self::is_bot_actor(actor_login, actor_type) {
            return out;
        }

        let issue_number = payload
            .get("issue")
            .and_then(|v| v.get("number"))
            .and_then(|v| v.as_u64());
        let Some(issue_number) = issue_number else {
            return out;
        };

        let issue_title = payload
            .get("issue")
            .and_then(|v| v.get("title"))
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let comment_url = comment
            .and_then(|v| v.get("html_url"))
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let timestamp = Self::parse_rfc3339_timestamp(
            comment
                .and_then(|v| v.get("created_at"))
                .and_then(|v| v.as_str()),
        );
        let comment_id = comment
            .and_then(|v| v.get("id"))
            .and_then(|v| v.as_u64())
            .map(|v| v.to_string());

        let sender = actor_login.unwrap_or("unknown");
        let content = format!(
            "[GitHub {event_name}] repo={repo} issue=#{issue_number} title=\"{issue_title}\"\n\
author={sender}\nurl={comment_url}\n\n{comment_body}"
        );

        out.push(ChannelMessage {
            id: Uuid::new_v4().to_string(),
            sender: sender.to_string(),
            reply_target: format!("{repo}#{issue_number}"),
            content,
            channel: "github".to_string(),
            timestamp,
            thread_ts: comment_id,
        });

        out
    }

    fn parse_pr_review_comment_event(&self, payload: &serde_json::Value) -> Vec<ChannelMessage> {
        let mut out = Vec::new();
        let action = payload
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        if action != "created" {
            return out;
        }

        let repo = payload
            .get("repository")
            .and_then(|v| v.get("full_name"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty());
        let Some(repo) = repo else {
            return out;
        };

        if !self.repo_is_allowed(repo) {
            tracing::warn!(
                "GitHub: ignoring webhook for unauthorized repository '{repo}'. \
                 Add repo to channels_config.github.allowed_repos or use '*' explicitly."
            );
            return out;
        }

        let comment = payload.get("comment");
        let comment_body = comment
            .and_then(|v| v.get("body"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty());
        let Some(comment_body) = comment_body else {
            return out;
        };

        let actor_login = comment
            .and_then(|v| v.get("user"))
            .and_then(|v| v.get("login"))
            .and_then(|v| v.as_str())
            .or_else(|| {
                payload
                    .get("sender")
                    .and_then(|v| v.get("login"))
                    .and_then(|v| v.as_str())
            });
        let actor_type = comment
            .and_then(|v| v.get("user"))
            .and_then(|v| v.get("type"))
            .and_then(|v| v.as_str())
            .or_else(|| {
                payload
                    .get("sender")
                    .and_then(|v| v.get("type"))
                    .and_then(|v| v.as_str())
            });

        if Self::is_bot_actor(actor_login, actor_type) {
            return out;
        }

        let pr_number = payload
            .get("pull_request")
            .and_then(|v| v.get("number"))
            .and_then(|v| v.as_u64());
        let Some(pr_number) = pr_number else {
            return out;
        };

        let pr_title = payload
            .get("pull_request")
            .and_then(|v| v.get("title"))
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let comment_url = comment
            .and_then(|v| v.get("html_url"))
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let file_path = comment
            .and_then(|v| v.get("path"))
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let timestamp = Self::parse_rfc3339_timestamp(
            comment
                .and_then(|v| v.get("created_at"))
                .and_then(|v| v.as_str()),
        );
        let comment_id = comment
            .and_then(|v| v.get("id"))
            .and_then(|v| v.as_u64())
            .map(|v| v.to_string());

        let sender = actor_login.unwrap_or("unknown");
        let content = format!(
            "[GitHub pull_request_review_comment] repo={repo} pr=#{pr_number} title=\"{pr_title}\"\n\
author={sender}\nfile={file_path}\nurl={comment_url}\n\n{comment_body}"
        );

        out.push(ChannelMessage {
            id: Uuid::new_v4().to_string(),
            sender: sender.to_string(),
            reply_target: format!("{repo}#{pr_number}"),
            content,
            channel: "github".to_string(),
            timestamp,
            thread_ts: comment_id,
        });

        out
    }

    pub fn parse_webhook_payload(
        &self,
        event_name: &str,
        payload: &serde_json::Value,
    ) -> Vec<ChannelMessage> {
        match event_name {
            "issue_comment" => self.parse_issue_comment_event(payload, event_name),
            "pull_request_review_comment" => self.parse_pr_review_comment_event(payload),
            _ => Vec::new(),
        }
    }
}

#[async_trait]
impl Channel for GitHubChannel {
    fn name(&self) -> &str {
        "github"
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        let Some((repo, issue_number)) = Self::parse_issue_recipient(&message.recipient) else {
            anyhow::bail!(
                "GitHub recipient must be in 'owner/repo#number' format, got '{}'",
                message.recipient
            );
        };

        if !self.repo_is_allowed(repo) {
            anyhow::bail!("GitHub repository '{repo}' is not in allowed_repos");
        }

        self.post_issue_comment(repo, issue_number, &message.content)
            .await
    }

    async fn listen(&self, _tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        tracing::info!(
            "GitHub channel active (webhook mode). \
            Configure GitHub webhook to POST to your gateway's /github endpoint."
        );

        loop {
            tokio::time::sleep(Duration::from_secs(3600)).await;
        }
    }

    async fn health_check(&self) -> bool {
        let url = format!("{}/rate_limit", self.api_base_url);
        self.client
            .get(&url)
            .bearer_auth(&self.access_token)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", GITHUB_API_VERSION)
            .header("User-Agent", "ZeroClaw-GitHub-Channel")
            .send()
            .await
            .map(|resp| resp.status().is_success())
            .unwrap_or(false)
    }
}

/// Verify a GitHub webhook signature from `X-Hub-Signature-256`.
///
/// GitHub sends signatures as `sha256=<hex_hmac>` over the raw request body.
pub fn verify_github_signature(secret: &str, body: &[u8], signature_header: &str) -> bool {
    let signature_hex = signature_header
        .trim()
        .strip_prefix("sha256=")
        .unwrap_or("")
        .trim();
    if signature_hex.is_empty() {
        return false;
    }
    let Ok(expected) = hex::decode(signature_hex) else {
        return false;
    };
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret.as_bytes()) else {
        return false;
    };
    mac.update(body);
    mac.verify_slice(&expected).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_channel() -> GitHubChannel {
        GitHubChannel::new(
            "ghp_test".to_string(),
            None,
            vec!["zeroclaw-labs/zeroclaw".to_string()],
        )
    }

    #[test]
    fn github_channel_name() {
        let ch = make_channel();
        assert_eq!(ch.name(), "github");
    }

    #[test]
    fn verify_github_signature_valid() {
        let secret = "test_secret";
        let body = br#"{"action":"created"}"#;
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        let signature = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));
        assert!(verify_github_signature(secret, body, &signature));
    }

    #[test]
    fn verify_github_signature_rejects_invalid() {
        assert!(!verify_github_signature("secret", b"{}", "sha256=deadbeef"));
        assert!(!verify_github_signature("secret", b"{}", ""));
    }

    #[test]
    fn parse_issue_comment_event_created() {
        let ch = make_channel();
        let payload = serde_json::json!({
            "action": "created",
            "repository": { "full_name": "zeroclaw-labs/zeroclaw" },
            "issue": { "number": 2079, "title": "GitHub as a native channel" },
            "comment": {
                "id": 12345,
                "body": "please add this",
                "created_at": "2026-02-27T14:00:00Z",
                "html_url": "https://github.com/zeroclaw-labs/zeroclaw/issues/2079#issuecomment-12345",
                "user": { "login": "alice", "type": "User" }
            }
        });
        let msgs = ch.parse_webhook_payload("issue_comment", &payload);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].reply_target, "zeroclaw-labs/zeroclaw#2079");
        assert_eq!(msgs[0].sender, "alice");
        assert_eq!(msgs[0].thread_ts.as_deref(), Some("12345"));
        assert!(msgs[0].content.contains("please add this"));
    }

    #[test]
    fn parse_issue_comment_event_skips_bot_actor() {
        let ch = make_channel();
        let payload = serde_json::json!({
            "action": "created",
            "repository": { "full_name": "zeroclaw-labs/zeroclaw" },
            "issue": { "number": 1, "title": "x" },
            "comment": {
                "id": 3,
                "body": "bot note",
                "user": { "login": "zeroclaw-bot[bot]", "type": "Bot" }
            }
        });
        let msgs = ch.parse_webhook_payload("issue_comment", &payload);
        assert!(msgs.is_empty());
    }

    #[test]
    fn parse_issue_comment_event_blocks_unallowed_repo() {
        let ch = make_channel();
        let payload = serde_json::json!({
            "action": "created",
            "repository": { "full_name": "other/repo" },
            "issue": { "number": 1, "title": "x" },
            "comment": { "body": "hello", "user": { "login": "alice", "type": "User" } }
        });
        let msgs = ch.parse_webhook_payload("issue_comment", &payload);
        assert!(msgs.is_empty());
    }

    #[test]
    fn parse_pr_review_comment_event_created() {
        let ch = make_channel();
        let payload = serde_json::json!({
            "action": "created",
            "repository": { "full_name": "zeroclaw-labs/zeroclaw" },
            "pull_request": { "number": 2118, "title": "Add github channel" },
            "comment": {
                "id": 9001,
                "body": "nit: rename this variable",
                "path": "src/channels/github.rs",
                "created_at": "2026-02-27T14:00:00Z",
                "html_url": "https://github.com/zeroclaw-labs/zeroclaw/pull/2118#discussion_r9001",
                "user": { "login": "bob", "type": "User" }
            }
        });
        let msgs = ch.parse_webhook_payload("pull_request_review_comment", &payload);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].reply_target, "zeroclaw-labs/zeroclaw#2118");
        assert_eq!(msgs[0].sender, "bob");
        assert!(msgs[0].content.contains("nit: rename this variable"));
    }

    #[test]
    fn parse_issue_recipient_format() {
        assert_eq!(
            GitHubChannel::parse_issue_recipient("zeroclaw-labs/zeroclaw#12"),
            Some(("zeroclaw-labs/zeroclaw", 12))
        );
        assert!(GitHubChannel::parse_issue_recipient("bad").is_none());
        assert!(GitHubChannel::parse_issue_recipient("owner/repo#0").is_none());
    }

    #[test]
    fn allowlist_supports_wildcards() {
        let ch = GitHubChannel::new("t".into(), None, vec!["zeroclaw-labs/*".into()]);
        assert!(ch.repo_is_allowed("zeroclaw-labs/zeroclaw"));
        assert!(!ch.repo_is_allowed("other/repo"));
        let all = GitHubChannel::new("t".into(), None, vec!["*".into()]);
        assert!(all.repo_is_allowed("anything/repo"));
    }
}
