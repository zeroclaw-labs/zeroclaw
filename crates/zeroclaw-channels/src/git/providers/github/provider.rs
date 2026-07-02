//! `GithubProvider` — GitHub's implementation of the [`GitProvider`] seam.
//!
//! Layer: provider composition root. Wraps `auth` (JWT/token), `api`
//! (REST), and `mapping` (payload→event), and owns every GitHub-specific
//! quirk so the generic `git` core stays forge-agnostic: installation/slug
//! discovery, the releases endpoint's missing `since` filter, the
//! pending-run-aware workflow-runs cursor, and the events-feed
//! ETag/304/page≥100 backbone. The core calls these per poll tick and per
//! outbound message; the provider caches auth internally.

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use super::api::GithubApi;
use super::auth::AppAuth;
use super::mapping;
use super::payloads::InstallationId;
use crate::git::poll::{PollStream, runs_cursor_candidate};
use crate::git::traits::{FetchPage, GitProvider, ReactionTarget, SelfIdentity};
use crate::git::types::{GitChannelError, IssueRef, RepoRef};

pub struct GithubProvider {
    auth: AppAuth,
    api: GithubApi,
    /// Explicit installation id from config; `None` triggers discovery.
    configured_installation: Option<u64>,
    /// App slug resolved from `GET /app`, cached for `self_identity`.
    slug: parking_lot::Mutex<Option<String>>,
    /// Installation resolved from config or discovery, cached.
    installation: parking_lot::Mutex<Option<InstallationId>>,
}

impl GithubProvider {
    pub fn new(
        app_id: u64,
        private_key_path: String,
        installation_id: Option<u64>,
        proxy_url: Option<String>,
    ) -> Self {
        Self {
            auth: AppAuth::new(app_id, private_key_path),
            api: GithubApi::new(proxy_url),
            configured_installation: installation_id,
            slug: parking_lot::Mutex::new(None),
            installation: parking_lot::Mutex::new(None),
        }
    }

    /// Swap in an API client pointed at a mock server.
    #[cfg(test)]
    pub(crate) fn with_api(mut self, api: GithubApi) -> Self {
        self.api = api;
        self
    }

    /// Resolve the installation to act as: config wins, then the cached
    /// discovery result, then `GET /app/installations` (sole entry).
    async fn installation_id(&self) -> Result<InstallationId, GitChannelError> {
        if let Some(id) = self.configured_installation {
            return Ok(InstallationId(id));
        }
        if let Some(id) = *self.installation.lock() {
            return Ok(id);
        }
        let jwt = self.auth.mint_jwt()?;
        let installations = self.api.list_installations(&jwt).await?;
        let id = match installations.as_slice() {
            [] => return Err(GitChannelError::NoInstallation),
            [only] => InstallationId(only.id),
            many => return Err(GitChannelError::MultipleInstallations(many.len())),
        };
        *self.installation.lock() = Some(id);
        Ok(id)
    }

    /// A fresh-enough installation token, exchanging a new app JWT when
    /// the cached one is inside the refresh buffer.
    async fn token(&self) -> Result<String, GitChannelError> {
        if let Some(token) = self.auth.cached_token() {
            return Ok(token);
        }
        let installation = self.installation_id().await?;
        let jwt = self.auth.mint_jwt()?;
        let token = self
            .api
            .create_installation_token(&jwt, installation)
            .await?;
        self.auth.store_token(token.clone());
        Ok(token.token)
    }

    /// The app slug (users mention `@<slug>`; the bot's login is
    /// `<slug>[bot]`), resolved once via `GET /app`.
    async fn ensure_slug(&self) -> Result<String, GitChannelError> {
        if let Some(slug) = self.slug.lock().clone() {
            return Ok(slug);
        }
        let jwt = self.auth.mint_jwt()?;
        let slug = self.api.app_slug(&jwt).await?;
        *self.slug.lock() = Some(slug.clone());
        Ok(slug)
    }

    // ── Per-stream fetch helpers (own GitHub's endpoint quirks) ─────

    async fn fetch_issues(
        &self,
        token: &str,
        repo: &RepoRef,
        since: DateTime<Utc>,
    ) -> Result<FetchPage, GitChannelError> {
        let mut events = Vec::new();
        let mut advance_to: Option<DateTime<Utc>> = None;
        for issue in self.api.list_issues_since(token, repo, since).await? {
            events.push(mapping::from_issue_opened(&issue, repo));
            if let Some(transition) = mapping::from_pull_transition(&issue, repo) {
                events.push(transition);
            }
            // Advance on creation timestamps only: closes happen to old
            // items that sort first, and advancing past them could skip
            // unseen items behind a full page. The dedup set absorbs the
            // re-reads this leaves behind.
            advance_to = Some(advance_to.map_or(issue.created_at, |m| m.max(issue.created_at)));
        }
        Ok(FetchPage {
            events,
            advance_to,
            etag: None,
            not_modified: false,
        })
    }

    async fn fetch_comments(
        &self,
        token: &str,
        repo: &RepoRef,
        since: DateTime<Utc>,
    ) -> Result<FetchPage, GitChannelError> {
        let mut events = Vec::new();
        let mut advance_to: Option<DateTime<Utc>> = None;
        for comment in self
            .api
            .list_issue_comments_since(token, repo, since)
            .await?
        {
            if let Some(event) = mapping::from_comment(&comment, repo) {
                advance_to =
                    Some(advance_to.map_or(comment.created_at, |m| m.max(comment.created_at)));
                events.push(event);
            }
        }
        Ok(FetchPage {
            events,
            advance_to,
            etag: None,
            not_modified: false,
        })
    }

    async fn fetch_review_comments(
        &self,
        token: &str,
        repo: &RepoRef,
        since: DateTime<Utc>,
    ) -> Result<FetchPage, GitChannelError> {
        let mut events = Vec::new();
        let mut advance_to: Option<DateTime<Utc>> = None;
        for comment in self
            .api
            .list_review_comments_since(token, repo, since)
            .await?
        {
            if let Some(event) = mapping::from_review_comment(&comment, repo) {
                advance_to =
                    Some(advance_to.map_or(comment.created_at, |m| m.max(comment.created_at)));
                events.push(event);
            }
        }
        Ok(FetchPage {
            events,
            advance_to,
            etag: None,
            not_modified: false,
        })
    }

    async fn fetch_releases(
        &self,
        token: &str,
        repo: &RepoRef,
        since: DateTime<Utc>,
    ) -> Result<FetchPage, GitChannelError> {
        let mut events = Vec::new();
        let mut advance_to: Option<DateTime<Utc>> = None;
        for release in self.api.list_releases(token, repo).await? {
            // The endpoint has no `since` parameter; filter here.
            let Some(published_at) = release.published_at else {
                continue;
            };
            if published_at < since {
                continue;
            }
            if let Some(event) = mapping::from_release(&release, repo) {
                advance_to = Some(advance_to.map_or(published_at, |m| m.max(published_at)));
                events.push(event);
            }
        }
        Ok(FetchPage {
            events,
            advance_to,
            etag: None,
            not_modified: false,
        })
    }

    async fn fetch_workflow_runs(
        &self,
        token: &str,
        repo: &RepoRef,
        since: DateTime<Utc>,
    ) -> Result<FetchPage, GitChannelError> {
        let runs = self
            .api
            .list_workflow_runs_created_since(token, repo, since)
            .await?;
        let mut events = Vec::new();
        let mut completed_max: Option<DateTime<Utc>> = None;
        let mut pending_min: Option<DateTime<Utc>> = None;
        for run in &runs {
            if run.status == "completed" {
                completed_max =
                    Some(completed_max.map_or(run.created_at, |m| m.max(run.created_at)));
                if let Some(event) = mapping::from_workflow_run(run, repo) {
                    events.push(event);
                }
            } else {
                pending_min = Some(pending_min.map_or(run.created_at, |m| m.min(run.created_at)));
            }
        }
        Ok(FetchPage {
            events,
            // The pending-run-aware cursor: never pass a still-running run.
            advance_to: runs_cursor_candidate(completed_max, pending_min),
            etag: None,
            not_modified: false,
        })
    }

    async fn fetch_feed(
        &self,
        token: &str,
        repo: &RepoRef,
        etag: Option<&str>,
    ) -> Result<FetchPage, GitChannelError> {
        let page = self.api.list_repo_events(token, repo, etag).await?;
        if page.not_modified {
            return Ok(FetchPage {
                events: Vec::new(),
                advance_to: None,
                etag: None,
                not_modified: true,
            });
        }
        if page.events.len() >= 100 {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({"repo": repo.to_string()})),
                "events feed page full; feed-only events may have been missed \
                 (targeted endpoints still cover routed types)"
            );
        }
        let events = page
            .events
            .iter()
            .filter_map(|entry| mapping::from_repo_event(entry, repo))
            .collect();
        // The feed advances no event-time cursor: it dedups against the
        // targeted endpoints and tracks freshness purely via the ETag.
        Ok(FetchPage {
            events,
            advance_to: None,
            etag: page.etag,
            not_modified: false,
        })
    }
}

#[async_trait]
impl GitProvider for GithubProvider {
    fn name(&self) -> &'static str {
        "github"
    }

    async fn self_identity(&self) -> Result<SelfIdentity, GitChannelError> {
        let slug = self.ensure_slug().await?;
        Ok(SelfIdentity {
            bot_login: format!("{slug}[bot]"),
            mention_handle: slug,
        })
    }

    async fn discover_repos(&self) -> Result<Vec<RepoRef>, GitChannelError> {
        let token = self.token().await?;
        self.api.list_installation_repos(&token).await
    }

    async fn fetch(
        &self,
        repo: &RepoRef,
        stream: PollStream,
        since: DateTime<Utc>,
        etag: Option<&str>,
    ) -> Result<FetchPage, GitChannelError> {
        let token = self.token().await?;
        match stream {
            PollStream::Issues => self.fetch_issues(&token, repo, since).await,
            PollStream::Comments => self.fetch_comments(&token, repo, since).await,
            PollStream::ReviewComments => self.fetch_review_comments(&token, repo, since).await,
            PollStream::Releases => self.fetch_releases(&token, repo, since).await,
            PollStream::WorkflowRuns => self.fetch_workflow_runs(&token, repo, since).await,
            PollStream::Feed => self.fetch_feed(&token, repo, etag).await,
        }
    }

    async fn post_comment(&self, target: &IssueRef, body: &str) -> Result<String, GitChannelError> {
        let token = self.token().await?;
        let id = self.api.create_comment(&token, target, body).await?;
        Ok(id.to_string())
    }

    async fn edit_comment(
        &self,
        repo: &RepoRef,
        comment_id: &str,
        body: &str,
    ) -> Result<(), GitChannelError> {
        let id: u64 = comment_id
            .parse()
            .map_err(|_| GitChannelError::BadRecipient(comment_id.to_string()))?;
        let token = self.token().await?;
        self.api.update_comment(&token, repo, id, body).await
    }

    async fn delete_comment(
        &self,
        repo: &RepoRef,
        comment_id: &str,
    ) -> Result<(), GitChannelError> {
        let id: u64 = comment_id
            .parse()
            .map_err(|_| GitChannelError::BadRecipient(comment_id.to_string()))?;
        let token = self.token().await?;
        self.api.delete_comment(&token, repo, id).await
    }

    async fn add_reaction(
        &self,
        target: &ReactionTarget,
        emoji: &str,
    ) -> Result<(), GitChannelError> {
        // Best-effort: unmappable emoji are dropped (matching the trait).
        let Some(content) = mapping::map_reaction(emoji) else {
            return Ok(());
        };
        let token = self.token().await?;
        match target {
            ReactionTarget::Comment { repo, comment_id } => {
                let Ok(id) = comment_id.parse::<u64>() else {
                    return Ok(());
                };
                self.api
                    .add_comment_reaction(&token, repo, id, content)
                    .await
            }
            ReactionTarget::Issue(issue) => {
                self.api.add_issue_reaction(&token, issue, content).await
            }
        }
    }
}
