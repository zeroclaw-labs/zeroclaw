//! GitHub REST payloads and provider-local constants.

use chrono::{DateTime, Utc};
use serde::Deserialize;

/// GitHub REST API base URL.
pub const GITHUB_API_BASE: &str = "https://api.github.com";
/// `Accept` header value for all GitHub REST requests.
pub const GITHUB_ACCEPT: &str = "application/vnd.github+json";
/// Pinned `X-GitHub-Api-Version` header value.
pub const GITHUB_API_VERSION: &str = "2022-11-28";
/// GitHub rejects requests without a `User-Agent`.
pub const GITHUB_USER_AGENT: &str = "zeroclaw";

/// Refresh installation tokens this many seconds before they expire.
pub const TOKEN_REFRESH_BUFFER_SECS: i64 = 60;

/// A GitHub App installation identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct InstallationId(pub u64);

impl std::fmt::Display for InstallationId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// Comment or issue author.
#[derive(Debug, Clone, Deserialize)]
pub struct GhUser {
    pub login: String,
    /// `"User"`, `"Bot"`, or `"Organization"`.
    #[serde(rename = "type", default)]
    pub kind: String,
}

impl GhUser {
    pub fn is_bot(&self) -> bool {
        self.kind.eq_ignore_ascii_case("bot")
    }
}

/// An issue or pull request (the issues namespace covers both).
#[derive(Debug, Clone, Deserialize)]
pub struct GhIssue {
    pub id: u64,
    pub number: u64,
    pub title: String,
    #[serde(default)]
    pub body: Option<String>,
    pub user: GhUser,
    pub created_at: DateTime<Utc>,
    #[serde(default, rename = "updated_at")]
    pub updated_at_raw: Option<DateTime<Utc>>,
    #[serde(default)]
    pub closed_at: Option<DateTime<Utc>>,
    /// Present iff the item is a pull request; `merged_at` inside is set
    /// once the PR has been merged (vs closed unmerged).
    #[serde(default)]
    pub pull_request: Option<GhIssuePullStub>,
    #[serde(default)]
    pub html_url: String,
}

impl GhIssue {
    pub fn is_pull_request(&self) -> bool {
        self.pull_request.is_some()
    }

    /// The effective last-modified time: the payload's `updated_at`, or
    /// `created_at` when a fixture omits it (an un-updated item has
    /// `updated_at == created_at`).
    pub fn updated_at(&self) -> DateTime<Utc> {
        self.updated_at_raw.unwrap_or(self.created_at)
    }
}

/// The pull-request stub embedded in issues-namespace payloads.
#[derive(Debug, Clone, Deserialize)]
pub struct GhIssuePullStub {
    #[serde(default)]
    pub merged_at: Option<DateTime<Utc>>,
}

/// A comment on an issue or pull request.
#[derive(Debug, Clone, Deserialize)]
pub struct GhComment {
    pub id: u64,
    #[serde(default)]
    pub body: Option<String>,
    pub user: GhUser,
    pub created_at: DateTime<Utc>,
    /// API URL of the parent issue; the trailing segment is the issue number.
    pub issue_url: String,
}

impl GhComment {
    /// Issue number extracted from `issue_url`'s trailing path segment.
    pub fn issue_number(&self) -> Option<u64> {
        self.issue_url.rsplit('/').next()?.parse().ok()
    }
}

/// A pull-request review comment (inline diff comment,
/// `/repos/{owner}/{repo}/pulls/comments`).
#[derive(Debug, Clone, Deserialize)]
pub struct GhReviewComment {
    pub id: u64,
    #[serde(default)]
    pub body: Option<String>,
    pub user: GhUser,
    pub created_at: DateTime<Utc>,
    /// API URL of the parent pull request; the trailing segment is the
    /// PR number.
    pub pull_request_url: String,
}

impl GhReviewComment {
    /// PR number extracted from `pull_request_url`'s trailing path segment.
    pub fn pull_number(&self) -> Option<u64> {
        self.pull_request_url.rsplit('/').next()?.parse().ok()
    }
}

/// A release (`/repos/{owner}/{repo}/releases`).
#[derive(Debug, Clone, Deserialize)]
pub struct GhRelease {
    pub id: u64,
    pub tag_name: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
    pub author: GhUser,
    #[serde(default)]
    pub draft: bool,
    /// `None` while the release is still a draft.
    #[serde(default)]
    pub published_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub html_url: String,
}

/// One workflow run (`/repos/{owner}/{repo}/actions/runs`).
#[derive(Debug, Clone, Deserialize)]
pub struct GhWorkflowRun {
    pub id: u64,
    #[serde(default)]
    pub name: Option<String>,
    /// `queued`, `in_progress`, or `completed`.
    pub status: String,
    /// Set once `status == "completed"`: `success`, `failure`,
    /// `cancelled`, `timed_out`, `startup_failure`, `skipped`, ….
    #[serde(default)]
    pub conclusion: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub head_branch: Option<String>,
    #[serde(default)]
    pub run_number: u64,
    #[serde(default = "default_run_attempt")]
    pub run_attempt: u64,
    #[serde(default)]
    pub actor: Option<GhUser>,
    /// Pull requests associated with the run. Empty for runs triggered
    /// from forks, even when a PR exists.
    #[serde(default)]
    pub pull_requests: Vec<GhRunPull>,
    #[serde(default)]
    pub html_url: String,
}

fn default_run_attempt() -> u64 {
    1
}

/// Pull-request stub on a workflow run.
#[derive(Debug, Clone, Deserialize)]
pub struct GhRunPull {
    pub number: u64,
}

/// A pull request from the pulls namespace or an Events-API payload.
/// Note: its `id` space is distinct from the issue-view `id` of the same
/// PR — cross-transport identity must key on `owner/repo#number`.
#[derive(Debug, Clone, Deserialize)]
pub struct GhPull {
    pub number: u64,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub body: Option<String>,
    pub user: GhUser,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub closed_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub merged_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub html_url: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GhRepoEvent {
    /// `IssueCommentEvent`, `IssuesEvent`, `PullRequestEvent`, ….
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub payload: serde_json::Value,
}

/// A repository Events API page: either fresh events plus the ETag to
/// send next time, or a not-modified marker (HTTP 304).
#[derive(Debug, Clone)]
pub struct RepoEventsPage {
    pub events: Vec<GhRepoEvent>,
    pub etag: Option<String>,
    pub not_modified: bool,
}

/// A repository visible to the installation.
#[derive(Debug, Clone, Deserialize)]
pub struct GhRepo {
    pub full_name: String,
}

/// The app itself (`GET /app`).
#[derive(Debug, Clone, Deserialize)]
pub struct GhApp {
    pub slug: String,
}

/// One installation of the app (`GET /app/installations`).
#[derive(Debug, Clone, Deserialize)]
pub struct GhInstallation {
    pub id: u64,
}

/// Response of `POST /app/installations/{id}/access_tokens`.
#[derive(Debug, Clone, Deserialize)]
pub struct GhTokenResponse {
    pub token: String,
    pub expires_at: DateTime<Utc>,
}

/// A cached installation access token.
#[derive(Clone)]
pub struct CachedToken {
    pub token: String,
    pub expires_at: DateTime<Utc>,
}

impl std::fmt::Debug for CachedToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CachedToken")
            .field("token", &"***")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

impl CachedToken {
    /// Whether the token is still safely usable at `now` (refresh-buffer
    /// seconds before the hard expiry).
    pub fn is_fresh(&self, now: DateTime<Utc>) -> bool {
        self.expires_at - chrono::Duration::seconds(TOKEN_REFRESH_BUFFER_SECS) > now
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comment_issue_number_comes_from_issue_url() {
        let c = GhComment {
            id: 1,
            body: Some("hi".into()),
            user: GhUser {
                login: "u".into(),
                kind: "User".into(),
            },
            created_at: Utc::now(),
            issue_url: "https://api.github.com/repos/o/r/issues/77".into(),
        };
        assert_eq!(c.issue_number(), Some(77));
    }

    #[test]
    fn cached_token_freshness_respects_buffer() {
        let now = Utc::now();
        let fresh = CachedToken {
            token: "t".into(),
            expires_at: now + chrono::Duration::seconds(TOKEN_REFRESH_BUFFER_SECS + 5),
        };
        let stale = CachedToken {
            token: "t".into(),
            expires_at: now + chrono::Duration::seconds(TOKEN_REFRESH_BUFFER_SECS - 5),
        };
        assert!(fresh.is_fresh(now));
        assert!(!stale.is_fresh(now));
    }

    #[test]
    fn debug_redacts_cached_token() {
        let tok = CachedToken {
            token: "ghs_supersecretinstallationtoken".into(),
            expires_at: Utc::now(),
        };
        let out = format!("{tok:?}");
        assert!(
            !out.contains("ghs_supersecretinstallationtoken"),
            "Debug must not print the raw installation token"
        );
        assert!(out.contains("***"), "Debug must mask the token");
    }
}
