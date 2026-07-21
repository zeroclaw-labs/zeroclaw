use chrono::{DateTime, Utc};

/// Maximum characters per posted comment. GitHub caps bodies at 65536;
/// other forges are at least as generous, so this conservative floor (with
/// headroom for split markers) is safe across providers.
pub const COMMENT_MAX_CHARS: usize = 65_000;

// ── Normalized event-type keys (the routing-table vocabulary) ──────

pub const EVT_ISSUE_COMMENT_CREATED: &str = "issue_comment.created";
pub const EVT_ISSUES_OPENED: &str = "issues.opened";
pub const EVT_PULL_REQUEST_OPENED: &str = "pull_request.opened";
pub const EVT_PULL_REQUEST_CLOSED: &str = "pull_request.closed";
pub const EVT_PULL_REQUEST_MERGED: &str = "pull_request.merged";
pub const EVT_PR_REVIEW_COMMENT_CREATED: &str = "pull_request_review_comment.created";
pub const EVT_WORKFLOW_RUN_COMPLETED: &str = "workflow_run.completed";
pub const EVT_WORKFLOW_RUN_FAILED: &str = "workflow_run.failed";
pub const EVT_RELEASE_PUBLISHED: &str = "release.published";

/// Every event type the channel can surface — the valid keys of
/// `[channels.git.<alias>.events]`, used for config validation.
pub const KNOWN_EVENT_TYPES: &[&str] = &[
    EVT_ISSUE_COMMENT_CREATED,
    EVT_ISSUES_OPENED,
    EVT_PULL_REQUEST_OPENED,
    EVT_PULL_REQUEST_CLOSED,
    EVT_PULL_REQUEST_MERGED,
    EVT_PR_REVIEW_COMMENT_CREATED,
    EVT_WORKFLOW_RUN_COMPLETED,
    EVT_WORKFLOW_RUN_FAILED,
    EVT_RELEASE_PUBLISHED,
];

/// A repository reference (`owner/repo`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RepoRef {
    pub owner: String,
    pub repo: String,
}

impl RepoRef {
    /// Parse `owner/repo`. Returns `None` when either half is empty.
    pub fn parse(s: &str) -> Option<Self> {
        let (owner, repo) = s.split_once('/')?;
        if owner.is_empty() || repo.is_empty() || repo.contains('/') {
            return None;
        }
        Some(Self {
            owner: owner.to_string(),
            repo: repo.to_string(),
        })
    }
}

impl std::fmt::Display for RepoRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.owner, self.repo)
    }
}

/// HTTP verb for a low-level forge API call. Provider-neutral so the contract
/// carries no reqwest dependency; each provider maps this onto its own client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForgeMethod {
    Get,
    Post,
    Patch,
    Put,
    Delete,
}

impl ForgeMethod {
    /// Parse a case-insensitive HTTP verb, defaulting unknown input to `Get`
    /// is refused: callers get `None` so a bad verb never silently becomes a
    /// read.
    pub fn parse(s: &str) -> Option<Self> {
        let upper = s.trim().to_ascii_uppercase();
        [
            (Self::Get, "GET"),
            (Self::Post, "POST"),
            (Self::Patch, "PATCH"),
            (Self::Put, "PUT"),
            (Self::Delete, "DELETE"),
        ]
        .into_iter()
        .find(|(_, name)| *name == upper)
        .map(|(m, _)| m)
    }
}

/// A low-level, provider-relative forge API call. `path` is relative to the
/// provider's API base (e.g. `repos/owner/repo/issues/12/labels`); the provider
/// prepends its base URL and attaches auth. `body` is an optional JSON payload.
#[derive(Debug, Clone)]
pub struct ForgeRequest {
    pub method: ForgeMethod,
    pub path: String,
    pub body: Option<serde_json::Value>,
}

/// The outcome of a low-level forge call: the HTTP status and the decoded JSON
/// body (or `Null` when the response had no body). Non-2xx statuses are returned
/// here rather than raised, so the caller sees the forge's own error envelope.
#[derive(Debug, Clone)]
pub struct ForgeResponse {
    pub status: u16,
    pub body: serde_json::Value,
}

/// An issue or pull-request reference (`owner/repo#number`) — the
/// channel's `reply_target` / `recipient` wire format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueRef {
    pub repo: RepoRef,
    pub number: u64,
}

impl IssueRef {
    /// Parse `owner/repo#number`.
    pub fn parse(s: &str) -> Option<Self> {
        let (repo, number) = s.split_once('#')?;
        Some(Self {
            repo: RepoRef::parse(repo)?,
            number: number.parse().ok()?,
        })
    }
}

impl std::fmt::Display for IssueRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}#{}", self.repo, self.number)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum GitChannelError {
    #[error(
        "GitHub App private key is not configured; set `private_key` to the \
         RS256 PEM (the contents of the app's `.pem` file, including the \
         BEGIN/END lines)"
    )]
    MissingPrivateKey,
    #[error("git provider JWT error: {0}")]
    Jwt(#[from] jsonwebtoken::errors::Error),
    #[error("git API {endpoint} failed ({status}): {body}")]
    Api {
        endpoint: String,
        status: u16,
        body: String,
    },
    #[error("git API rate limited until {reset_at}")]
    RateLimited { reset_at: DateTime<Utc> },
    #[error(
        "git provider has no installations; install the app on a repository \
         or set `installation_id`"
    )]
    NoInstallation,
    #[error("git provider has {0} installations; set `installation_id` to choose one")]
    MultipleInstallations(usize),
    #[error("git provider configuration error: {0}")]
    Config(String),
    #[error("invalid git recipient `{0}` (expected `owner/repo#number`)")]
    BadRecipient(String),
    #[error(transparent)]
    Http(#[from] reqwest::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_ref_parses_owner_and_repo() {
        let r = RepoRef::parse("octo/repo").unwrap();
        assert_eq!(r.owner, "octo");
        assert_eq!(r.repo, "repo");
        assert_eq!(r.to_string(), "octo/repo");
    }

    #[test]
    fn repo_ref_rejects_malformed_input() {
        assert!(RepoRef::parse("no-slash").is_none());
        assert!(RepoRef::parse("/repo").is_none());
        assert!(RepoRef::parse("owner/").is_none());
        assert!(RepoRef::parse("a/b/c").is_none());
    }

    #[test]
    fn issue_ref_round_trips() {
        let i = IssueRef::parse("octo/repo#42").unwrap();
        assert_eq!(i.number, 42);
        assert_eq!(i.to_string(), "octo/repo#42");
    }

    #[test]
    fn issue_ref_rejects_bad_number_and_missing_hash() {
        assert!(IssueRef::parse("octo/repo").is_none());
        assert!(IssueRef::parse("octo/repo#abc").is_none());
    }
}
