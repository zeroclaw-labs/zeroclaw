//! `GitChannel` — the provider-agnostic composition root implementing the
//! `Channel` trait over a [`GitProvider`].

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};
use zeroclaw_config::schema::GitConfig;

use super::events::{self, EventFilter, GitEvent};
use super::poll::{PollState, PollStream};
use super::router::{self, RouteAction, TransportPlan};
use super::traits::{GitProvider, ReactionTarget, SelfIdentity};
use super::types::{
    COMMENT_MAX_CHARS, ForgeMethod, ForgeRequest, GitChannelError, IssueRef, RepoRef,
};

/// The channel key under `[channels.git.<alias>]` — also stamped on every
/// `ChannelMessage` as its `channel`.
const CHANNEL_KEY: &str = "git";

/// Floor for `poll_interval_secs` — protects the rate budget against
/// configs like `poll_interval_secs = 1`.
const MIN_POLL_INTERVAL_SECS: u64 = 15;

/// Minimum spacing between draft edits on one comment; forges' secondary
/// abuse limits punish rapid content mutation.
const DRAFT_EDIT_MIN_INTERVAL: Duration = Duration::from_secs(2);

/// Resolve the GitHub App private key PEM, preferring the inline `private_key`
/// and falling back to reading `private_key_path` from disk. Backward-compatible
/// with configs that predate the inline field.
fn resolve_github_private_key(cfg: &GitConfig) -> anyhow::Result<Option<String>> {
    if cfg.private_key.is_some() {
        return Ok(cfg.private_key.clone());
    }
    let Some(path) = cfg
        .private_key_path
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
    else {
        return Ok(None);
    };
    match std::fs::read_to_string(path) {
        Ok(pem) => {
            warn_on_loose_permissions(path);
            Ok(Some(pem))
        }
        Err(e) => anyhow::bail!("git channel: reading private_key_path `{path}` failed: {e}"),
    }
}

/// The private key is a long-lived credential: group/other access on the
/// key file is operator error worth surfacing, but not worth refusing to
/// start over.
#[cfg(unix)]
fn warn_on_loose_permissions(path: &str) {
    use std::os::unix::fs::MetadataExt;
    let Ok(meta) = std::fs::metadata(path) else {
        return;
    };
    if meta.mode() & 0o077 != 0 {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_attrs(::serde_json::json!({"path": path})),
            "GitHub App private key is readable by group/other; chmod 600 recommended"
        );
    }
}

#[cfg(not(unix))]
fn warn_on_loose_permissions(_path: &str) {}

/// Build the configured forge provider, or a clear error for an unknown
/// `provider` value. The only forge-aware seam in the channel.
fn build_provider(cfg: &GitConfig) -> anyhow::Result<Box<dyn GitProvider>> {
    match cfg.provider.trim().to_ascii_lowercase().as_str() {
        "" | "github" => {
            #[cfg(feature = "provider-github")]
            {
                Ok(Box::new(super::providers::github::GithubProvider::new(
                    cfg.app_id,
                    resolve_github_private_key(cfg)?,
                    cfg.installation_id,
                    cfg.proxy_url.clone(),
                )))
            }
            #[cfg(not(feature = "provider-github"))]
            {
                anyhow::bail!(
                    "git channel provider `github` requires the `provider-github` feature"
                );
            }
        }
        provider @ ("gitea" | "forgejo") => {
            #[cfg(feature = "provider-gitea")]
            {
                // Fail closed before any HTTP client exists: every request
                // attaches `access_token` as a bearer credential, so guessing
                // a default host would send the token to an endpoint the
                // operator never named (e.g. a Forgejo PAT to gitea.com).
                let Some(api_base_url) = cfg
                    .api_base_url
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                else {
                    anyhow::bail!(
                        "git channel provider `{provider}` requires \
                         channels.git.<alias>.api_base_url - the instance's API base \
                         URL including /api/v1, e.g. `https://git.example.org/api/v1` \
                         (or `https://gitea.com/api/v1` for the public Gitea service). \
                         No default host is assumed because API requests carry the \
                         access token"
                    );
                };
                Ok(Box::new(super::providers::gitea::GiteaProvider::new(
                    api_base_url.to_string(),
                    cfg.access_token.clone(),
                    cfg.proxy_url.clone(),
                )))
            }
            #[cfg(not(feature = "provider-gitea"))]
            {
                anyhow::bail!(
                    "git channel provider `{provider}` requires the `provider-gitea` feature"
                );
            }
        }
        other => anyhow::bail!(
            "unknown git channel provider `{other}` (supported: github, gitea, forgejo)"
        ),
    }
}

pub struct GitChannel {
    cfg: GitConfig,
    /// The alias key under `[channels.git.<alias>]` this handle is bound
    /// to. Used to scope peer-group lookups and session keys.
    alias: String,
    /// Resolves inbound external peers from canonical state at message-time.
    /// No cache (see AGENTS.md "ABSOLUTE RULE — SINGLE SOURCE OF TRUTH").
    peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    /// The forge driver. Owns its own auth/token/identity caches.
    provider: Box<dyn GitProvider>,
    /// Bot identity (mention handle + bot login) resolved from the
    /// provider, cached for the sync `self_handle`/`self_addressed_mention`
    /// accessors. The provider remains the source of truth; this is a
    /// memo of the value it returned.
    identity: parking_lot::Mutex<Option<SelfIdentity>>,
    /// Last draft-edit instant per comment id (throttle).
    draft_edits: parking_lot::Mutex<HashMap<String, Instant>>,
}

impl GitChannel {
    pub fn new(
        cfg: GitConfig,
        alias: impl Into<String>,
        peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    ) -> anyhow::Result<Self> {
        let provider = build_provider(&cfg)?;
        Ok(Self {
            cfg,
            alias: alias.into(),
            peer_resolver,
            provider,
            identity: parking_lot::Mutex::new(None),
            draft_edits: parking_lot::Mutex::new(HashMap::new()),
        })
    }

    #[cfg(test)]
    fn with_provider(
        cfg: GitConfig,
        alias: impl Into<String>,
        peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
        provider: Box<dyn GitProvider>,
    ) -> Self {
        Self {
            cfg,
            alias: alias.into(),
            peer_resolver,
            provider,
            identity: parking_lot::Mutex::new(None),
            draft_edits: parking_lot::Mutex::new(HashMap::new()),
        }
    }

    /// Return the alias under `[channels.git.<alias>]` that this channel
    /// handle is bound to.
    pub fn alias(&self) -> &str {
        &self.alias
    }

    fn is_user_allowed(&self, login: &str) -> bool {
        let peers = (self.peer_resolver)();
        // Forge logins are case-insensitive (and ASCII-only).
        crate::allowlist::is_user_allowed(&peers, login, crate::allowlist::Match::CaseInsensitive)
    }

    /// Resolve the bot identity from the provider, memoizing the value for
    /// the sync accessors.
    async fn ensure_identity(&self) -> Result<(String, String), GitChannelError> {
        if let Some(id) = self.identity.lock().as_ref() {
            return Ok((id.mention_handle.clone(), id.bot_login.clone()));
        }
        let id = self.provider.self_identity().await?;
        let pair = (id.mention_handle.clone(), id.bot_login.clone());
        *self.identity.lock() = Some(id);
        Ok(pair)
    }

    /// Repos to poll: explicit config, else everything the provider can
    /// discover.
    async fn resolve_repos(&self) -> Result<Vec<RepoRef>, GitChannelError> {
        if !self.cfg.repos.is_empty() {
            let mut repos = Vec::with_capacity(self.cfg.repos.len());
            for entry in &self.cfg.repos {
                match RepoRef::parse(entry) {
                    Some(repo) => repos.push(repo),
                    None => {
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            )
                            .with_attrs(::serde_json::json!({"entry": entry})),
                            "ignoring malformed `repos` entry (expected owner/repo)"
                        );
                    }
                }
            }
            return Ok(repos);
        }
        self.provider.discover_repos().await
    }

    fn parse_recipient(recipient: &str) -> Result<IssueRef, GitChannelError> {
        IssueRef::parse(recipient)
            .ok_or_else(|| GitChannelError::BadRecipient(recipient.to_string()))
    }

    /// Which streams to fetch from the routing-derived plan plus the feed
    /// toggle. The provider interprets each stream against its own API.
    fn active_streams(&self, plan: &TransportPlan) -> Vec<PollStream> {
        let mut streams = Vec::new();
        if plan.issues {
            streams.push(PollStream::Issues);
        }
        if plan.comments {
            streams.push(PollStream::Comments);
        }
        if plan.review_comments {
            streams.push(PollStream::ReviewComments);
        }
        if plan.releases {
            streams.push(PollStream::Releases);
        }
        if plan.workflow_runs {
            streams.push(PollStream::WorkflowRuns);
        }
        if self.cfg.events_backbone {
            streams.push(PollStream::Feed);
        }
        streams
    }

    async fn poll_repo(
        &self,
        repo: &RepoRef,
        filter: &EventFilter<'_>,
        plan: &TransportPlan,
        state: &mut PollState,
        tx: &tokio::sync::mpsc::Sender<ChannelMessage>,
    ) -> Result<bool, GitChannelError> {
        let repo_key = repo.to_string();
        let mut batch: Vec<GitEvent> = Vec::new();
        let mut advances: Vec<(PollStream, DateTime<Utc>)> = Vec::new();
        let mut fresh_etag: Option<String> = None;
        // Dedup across streams within this tick, since the shared seen-set
        // is not committed until dispatch: an item surfaced by both a
        // targeted endpoint and the feed backbone is still admitted once.
        let mut this_tick: HashSet<String> = HashSet::new();

        for stream in self.active_streams(plan) {
            let since = state.since(&repo_key, stream);
            let etag = if stream == PollStream::Feed {
                state.etag(&repo_key).map(str::to_string)
            } else {
                None
            };
            let page = self
                .provider
                .fetch(repo, stream, since, etag.as_deref())
                .await?;
            if page.not_modified {
                continue;
            }
            if let Some(etag) = page.etag {
                fresh_etag = Some(etag);
            }
            if let Some(advance_to) = page.advance_to {
                advances.push((stream, advance_to));
            }
            for event in page.events {
                let id = event.dedup_id();
                // Read-only freshness check + within-tick dedup; the shared
                // seen-set is committed per event only after it dispatches.
                if state.is_fresh(&id, event.created_at()) && this_tick.insert(id) {
                    batch.push(event);
                }
            }
        }

        batch.sort_by_key(GitEvent::created_at);
        for event in batch {
            let id = event.dedup_id();
            if !self.dispatch_event(event, filter, tx).await {
                // Receiver hung up (shutdown): commit nothing further, so
                // the undelivered tail is re-fetched next run, and stop.
                return Ok(false);
            }
            state.mark_seen(&id);
        }

        // Every event was delivered: it is now safe to advance the stream
        // cursors and remember the feed ETag.
        if let Some(etag) = fresh_etag {
            state.set_etag(&repo_key, etag);
        }
        for (stream, advance_to) in advances {
            state.advance(&repo_key, stream, advance_to);
        }
        Ok(true)
    }

    async fn dispatch_event(
        &self,
        event: GitEvent,
        filter: &EventFilter<'_>,
        tx: &tokio::sync::mpsc::Sender<ChannelMessage>,
    ) -> bool {
        let msg = match router::resolve_route(event.event_type(), &self.cfg.events) {
            RouteAction::Ignore => return true,
            RouteAction::Message => {
                events::event_to_message(&event, filter, CHANNEL_KEY, &self.alias, true)
            }
            RouteAction::Sop { sop } => {
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_attrs(::serde_json::json!({
                            "sop": sop,
                            "event_type": event.event_type(),
                        })),
                    "git event routed to SOP ingress"
                );
                events::event_to_sop_message(
                    &event,
                    filter,
                    CHANNEL_KEY,
                    &self.alias,
                    &self.cfg.provider,
                    &sop,
                )
            }
        };
        let Some(msg) = msg else {
            return true;
        };
        if !self.is_user_allowed(&msg.sender) {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({"sender": msg.sender})),
                "ignoring git event from unauthorized user"
            );
            return true;
        }
        tx.send(msg).await.is_ok()
    }

    async fn edit_comment(
        &self,
        recipient: &str,
        message_id: &str,
        text: &str,
        throttle: bool,
    ) -> anyhow::Result<()> {
        let issue = Self::parse_recipient(recipient)?;
        if throttle {
            let mut edits = self.draft_edits.lock();
            if let Some(last) = edits.get(message_id)
                && last.elapsed() < DRAFT_EDIT_MIN_INTERVAL
            {
                // Drop this intermediate update; the next one (or
                // finalize) carries the accumulated content anyway.
                return Ok(());
            }
            edits.insert(message_id.to_string(), Instant::now());
        }
        self.provider
            .edit_comment(&issue.repo, message_id, text)
            .await?;
        Ok(())
    }
}

impl ::zeroclaw_api::attribution::Attributable for GitChannel {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Channel(::zeroclaw_api::attribution::ChannelKind::Git)
    }
    fn alias(&self) -> &str {
        &self.alias
    }
}

#[async_trait]
impl Channel for GitChannel {
    fn name(&self) -> &str {
        CHANNEL_KEY
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        let issue = Self::parse_recipient(&message.recipient)?;
        for chunk in split_comment_text(&message.content, COMMENT_MAX_CHARS) {
            self.provider.post_comment(&issue, &chunk).await?;
        }
        Ok(())
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        let (mention_handle, bot_login) = self.ensure_identity().await?;
        let repos = self.resolve_repos().await?;
        let interval = Duration::from_secs(self.cfg.poll_interval_secs.max(MIN_POLL_INTERVAL_SECS));

        for problem in router::validate_routes(&self.cfg.events) {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({"problem": problem})),
                "git channel `events` routing entry can never fire"
            );
        }
        let plan = TransportPlan::from_routes(&self.cfg.events);

        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(
                ::serde_json::json!({
                    "provider": self.provider.name(),
                    "bot": mention_handle,
                    "repos": repos.iter().map(ToString::to_string).collect::<Vec<_>>(),
                    "poll_interval_secs": interval.as_secs(),
                    "mention_only": self.cfg.mention_only,
                    "transports": {
                        "issues": plan.issues,
                        "comments": plan.comments,
                        "review_comments": plan.review_comments,
                        "releases": plan.releases,
                        "workflow_runs": plan.workflow_runs,
                        "events_backbone": self.cfg.events_backbone,
                    },
                })
            ),
            "git channel polling"
        );
        if repos.is_empty() {
            anyhow::bail!(
                "git channel has no repositories to poll; set `repos` or grant the \
                 provider access to at least one repository"
            );
        }
        if !plan.any() {
            anyhow::bail!(
                "git channel `events` routing table ignores every event type; \
                 nothing would ever be delivered"
            );
        }

        let filter = EventFilter {
            bot_login: &bot_login,
            mention_handle: &mention_handle,
            mention_only: self.cfg.mention_only,
            listen_to_bots: self.cfg.listen_to_bots,
        };
        let mut state = PollState::new(chrono::Utc::now());

        loop {
            for repo in &repos {
                match self.poll_repo(repo, &filter, &plan, &mut state, &tx).await {
                    Ok(true) => {}
                    Ok(false) => return Ok(()),
                    Err(GitChannelError::RateLimited { reset_at }) => {
                        let wait = (reset_at - chrono::Utc::now())
                            .to_std()
                            .unwrap_or(Duration::from_secs(60))
                            // Jitter so multiple repos/instances don't
                            // stampede the moment the window resets.
                            + Duration::from_millis(u64::from(rand::random::<u16>()) % 5_000);
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            )
                            .with_attrs(::serde_json::json!({"wait_secs": wait.as_secs()})),
                            "git provider rate limited; backing off"
                        );
                        tokio::time::sleep(wait).await;
                        break;
                    }
                    Err(e) => {
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "repo": repo.to_string(),
                                "error": e.to_string(),
                            })),
                            "git poll failed for repo; continuing"
                        );
                    }
                }
            }

            tokio::time::sleep(interval).await;
        }
    }

    async fn health_check(&self) -> bool {
        self.ensure_identity().await.is_ok()
    }

    fn self_handle(&self) -> Option<String> {
        self.identity.lock().as_ref().map(|id| id.bot_login.clone())
    }

    fn self_addressed_mention(&self) -> Option<String> {
        self.identity
            .lock()
            .as_ref()
            .map(|id| format!("@{}", id.mention_handle))
    }

    fn supports_draft_updates(&self) -> bool {
        true
    }

    async fn send_draft(&self, message: &SendMessage) -> anyhow::Result<Option<String>> {
        let issue = Self::parse_recipient(&message.recipient)?;
        let id = self.provider.post_comment(&issue, &message.content).await?;
        self.draft_edits.lock().insert(id.clone(), Instant::now());
        Ok(Some(id))
    }

    async fn update_draft(
        &self,
        recipient: &str,
        message_id: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        self.edit_comment(recipient, message_id, text, true).await
    }

    async fn update_draft_progress(
        &self,
        recipient: &str,
        message_id: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        self.edit_comment(recipient, message_id, text, true).await
    }

    async fn finalize_draft(
        &self,
        recipient: &str,
        message_id: &str,
        text: &str,
        _suppress_voice: bool,
    ) -> anyhow::Result<()> {
        let result = self.edit_comment(recipient, message_id, text, false).await;
        self.draft_edits.lock().remove(message_id);
        result
    }

    async fn cancel_draft(&self, recipient: &str, message_id: &str) -> anyhow::Result<()> {
        let issue = Self::parse_recipient(recipient)?;
        self.provider
            .delete_comment(&issue.repo, message_id)
            .await?;
        self.draft_edits.lock().remove(message_id);
        Ok(())
    }

    /// Reactions are best-effort: unmappable emoji and unparsable targets
    /// are dropped silently, matching the trait's no-op default.
    async fn add_reaction(
        &self,
        channel_id: &str,
        message_id: &str,
        emoji: &str,
    ) -> anyhow::Result<()> {
        let Some(issue) = IssueRef::parse(channel_id) else {
            return Ok(());
        };
        // `ghc_<id>` message ids name a comment; anything else reacts on
        // the issue/PR body itself.
        let target = match message_id.strip_prefix("ghc_") {
            Some(comment_id) => ReactionTarget::Comment {
                repo: issue.repo.clone(),
                comment_id: comment_id.to_string(),
            },
            None => ReactionTarget::Issue(issue),
        };
        self.provider.add_reaction(&target, emoji).await?;
        Ok(())
    }

    async fn forge_request(
        &self,
        request: zeroclaw_api::channel::ForgeApiRequest,
    ) -> anyhow::Result<zeroclaw_api::channel::ForgeApiResponse> {
        let Some(method) = ForgeMethod::parse(&request.method) else {
            anyhow::bail!(
                "invalid forge HTTP method `{}` (expected GET/POST/PATCH/PUT/DELETE)",
                request.method
            );
        };
        let resp = self
            .provider
            .forge_request(ForgeRequest {
                method,
                path: request.path,
                body: request.body,
            })
            .await?;
        Ok(zeroclaw_api::channel::ForgeApiResponse {
            status: resp.status,
            body: resp.body,
        })
    }
}

/// Split text into comment-sized chunks at paragraph (preferred) or word
/// boundaries.
fn split_comment_text(text: &str, max_len: usize) -> Vec<String> {
    if text.len() <= max_len {
        return vec![text.to_string()];
    }

    let mut chunks = Vec::new();
    let mut remaining = text;
    while !remaining.is_empty() {
        if remaining.len() <= max_len {
            chunks.push(remaining.to_string());
            break;
        }
        let limit = crate::util::floor_char_boundary(remaining, max_len);
        let split_at = remaining[..limit]
            .rfind("\n\n")
            .or_else(|| remaining[..limit].rfind('\n'))
            .or_else(|| remaining[..limit].rfind(' '))
            .unwrap_or(limit);
        let split_at = split_at.max(1);
        chunks.push(remaining[..split_at].to_string());
        remaining = remaining[split_at..].trim_start();
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel(peers: Vec<String>) -> GitChannel {
        GitChannel::new(
            GitConfig::default(),
            "git_test_alias",
            Arc::new(move || peers.clone()),
        )
        .unwrap()
    }

    #[test]
    fn name_and_alias() {
        let ch = channel(vec![]);
        assert_eq!(ch.name(), "git");
        assert_eq!(ch.alias(), "git_test_alias");
    }

    #[test]
    fn resolve_key_prefers_inline_then_path() {
        let inline = GitConfig {
            private_key: Some("INLINE-PEM".to_string()),
            private_key_path: Some("/does/not/matter".to_string()),
            ..GitConfig::default()
        };
        assert_eq!(
            resolve_github_private_key(&inline).unwrap().as_deref(),
            Some("INLINE-PEM")
        );

        let mut path = std::env::temp_dir();
        path.push(format!("git_key_{}.pem", std::process::id()));
        std::fs::write(&path, "PATH-PEM").unwrap();
        let from_path = GitConfig {
            private_key: None,
            private_key_path: Some(path.to_string_lossy().into_owned()),
            ..GitConfig::default()
        };
        assert_eq!(
            resolve_github_private_key(&from_path).unwrap().as_deref(),
            Some("PATH-PEM")
        );
        let _ = std::fs::remove_file(&path);

        let neither = GitConfig::default();
        assert!(resolve_github_private_key(&neither).unwrap().is_none());

        let missing = GitConfig {
            private_key: None,
            private_key_path: Some("/no/such/key.pem".to_string()),
            ..GitConfig::default()
        };
        assert!(resolve_github_private_key(&missing).is_err());
    }

    #[test]
    fn unknown_provider_is_a_clear_error() {
        let cfg = GitConfig {
            provider: "bitbucket".to_string(),
            ..GitConfig::default()
        };
        let result = GitChannel::new(cfg, "main", Arc::new(Vec::new));
        let err = match result {
            Ok(_) => panic!("expected an error for an unknown provider"),
            Err(e) => e,
        };
        let msg = err.to_string();
        assert!(msg.contains("unknown git channel provider `bitbucket`"));
        // This slice ships the Gitea/Forgejo provider alongside GitHub; the
        // error must advertise exactly the providers the build accepts.
        assert!(msg.contains("supported: github, gitea, forgejo"));
    }

    #[cfg(feature = "provider-gitea")]
    #[test]
    fn gitea_forgejo_without_api_base_url_fails_closed() {
        for provider in ["gitea", "forgejo"] {
            for api_base_url in [None, Some("   ".to_string())] {
                let cfg = GitConfig {
                    provider: provider.to_string(),
                    access_token: "s3cr3t-tok".to_string(),
                    api_base_url: api_base_url.clone(),
                    ..GitConfig::default()
                };
                let err = match GitChannel::new(cfg, "main", Arc::new(Vec::new)) {
                    Ok(_) => {
                        panic!("`{provider}` with api_base_url {api_base_url:?} must not construct")
                    }
                    Err(e) => e,
                };
                let msg = err.to_string();
                assert!(msg.contains("api_base_url"), "{msg}");
                assert!(msg.contains(provider), "{msg}");
                assert!(
                    !msg.contains("s3cr3t-tok"),
                    "error must not echo the token: {msg}"
                );
            }
        }
    }

    #[test]
    fn self_handle_unknown_until_identity_resolved() {
        let ch = channel(vec![]);
        assert!(ch.self_handle().is_none());
        *ch.identity.lock() = Some(SelfIdentity {
            mention_handle: "myapp".into(),
            bot_login: "myapp[bot]".into(),
        });
        assert_eq!(ch.self_handle().as_deref(), Some("myapp[bot]"));
        assert_eq!(ch.self_addressed_mention().as_deref(), Some("@myapp"));
    }

    #[test]
    fn user_allowlist_is_case_insensitive_like_forge_logins() {
        let ch = channel(vec!["*".into()]);
        assert!(ch.is_user_allowed("anyone"));
        let ch = channel(vec!["Test_User".into()]);
        assert!(ch.is_user_allowed("test_user"));
        assert!(ch.is_user_allowed("TEST_USER"));
        assert!(!ch.is_user_allowed("mallory"));
        let ch = channel(vec![]);
        assert!(!ch.is_user_allowed("anyone"));
    }

    #[test]
    fn recipient_parser_rejects_garbage() {
        assert!(GitChannel::parse_recipient("octo/repo#3").is_ok());
        assert!(GitChannel::parse_recipient("octo/repo").is_err());
        assert!(GitChannel::parse_recipient("nonsense").is_err());
    }

    #[test]
    fn split_comment_text_prefers_paragraph_boundaries() {
        let text = format!("{}\n\n{}", "a".repeat(60), "b".repeat(60));
        let chunks = split_comment_text(&text, 100);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0], "a".repeat(60));
        assert_eq!(chunks[1], "b".repeat(60));
    }

    #[test]
    fn split_comment_text_short_passthrough_and_multibyte_safety() {
        assert_eq!(split_comment_text("hi", 100), vec!["hi"]);
        let text = format!("{}{}tail", "a".repeat(99), "😀");
        let chunks = split_comment_text(&text, 100);
        assert_eq!(chunks.concat().replace(' ', ""), text.replace(' ', ""));
        for chunk in &chunks {
            assert!(chunk.is_char_boundary(chunk.len()));
        }
    }

    #[cfg(feature = "provider-github")]
    mod github_integration {
        use super::*;
        use crate::git::providers::github::api_test_support::with_base;
        use crate::git::providers::github::{GithubProvider, TEST_KEY_PEM};

        fn base_cfg() -> GitConfig {
            GitConfig {
                enabled: true,
                app_id: 1,
                private_key: Some(TEST_KEY_PEM.to_string()),
                installation_id: Some(77),
                repos: vec!["octo/repo".into()],
                ..GitConfig::default()
            }
        }

        /// A `GitChannel` whose GitHub provider points at the mock server.
        fn mock_channel(cfg: GitConfig, server_uri: String) -> GitChannel {
            let provider = GithubProvider::new(
                cfg.app_id,
                cfg.private_key.clone(),
                cfg.installation_id,
                None,
            )
            .with_api(with_base(server_uri));
            GitChannel::with_provider(
                cfg,
                "main",
                Arc::new(|| vec!["*".into()]),
                Box::new(provider),
            )
        }

        fn test_filter() -> EventFilter<'static> {
            EventFilter {
                bot_login: "myapp[bot]",
                mention_handle: "myapp",
                mention_only: true,
                listen_to_bots: false,
            }
        }

        async fn mount_token_mock(server: &wiremock::MockServer) {
            use wiremock::matchers::{method, path};
            wiremock::Mock::given(method("POST"))
                .and(path("/app/installations/77/access_tokens"))
                .respond_with(
                    wiremock::ResponseTemplate::new(201).set_body_json(serde_json::json!({
                        "token": "ghs_test",
                        "expires_at": (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339(),
                    })),
                )
                .mount(server)
                .await;
        }

        fn default_plan() -> TransportPlan {
            TransportPlan::from_routes(&HashMap::new())
        }

        async fn mount_empty(server: &wiremock::MockServer, route: &str) {
            use wiremock::matchers::{method, path};
            wiremock::Mock::given(method("GET"))
                .and(path(route))
                .respond_with(
                    wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!([])),
                )
                .mount(server)
                .await;
        }

        #[tokio::test]
        async fn full_tick_polls_maps_and_forwards() {
            use wiremock::matchers::{method, path};
            use wiremock::{Mock, MockServer, ResponseTemplate};

            let server = MockServer::start().await;
            let now = chrono::Utc::now();
            mount_token_mock(&server).await;
            Mock::given(method("GET"))
                .and(path("/repos/octo/repo/issues"))
                .respond_with(
                    ResponseTemplate::new(200).set_body_json(serde_json::json!([{
                        "id": 555,
                        "number": 12,
                        "title": "Flaky test",
                        "body": "@myapp please investigate",
                        "user": {"login": "test_user", "type": "User"},
                        "created_at": (now - chrono::Duration::seconds(60)).to_rfc3339(),
                    }])),
                )
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/repos/octo/repo/issues/comments"))
                .respond_with(
                    ResponseTemplate::new(200).set_body_json(serde_json::json!([{
                        "id": 9001,
                        "body": "@myapp ping",
                        "user": {"login": "test_user", "type": "User"},
                        "created_at": (now - chrono::Duration::seconds(30)).to_rfc3339(),
                        "issue_url": "https://api.github.com/repos/octo/repo/issues/12",
                    }])),
                )
                .mount(&server)
                .await;

            let ch = mock_channel(base_cfg(), server.uri());

            let filter = test_filter();
            let plan = default_plan();
            let mut state = PollState::new(now - chrono::Duration::hours(1));
            let repo = RepoRef::parse("octo/repo").unwrap();
            let (tx, mut rx) = tokio::sync::mpsc::channel(8);
            let keep = ch
                .poll_repo(&repo, &filter, &plan, &mut state, &tx)
                .await
                .unwrap();
            assert!(keep);
            drop(tx);

            let first = rx.recv().await.unwrap();
            assert_eq!(first.id, "ghi_555");
            assert_eq!(first.subject.as_deref(), Some("Flaky test"));
            assert_eq!(first.content, "please investigate");
            assert_eq!(first.channel, "git");
            let second = rx.recv().await.unwrap();
            assert_eq!(second.id, "ghc_9001");
            assert_eq!(second.content, "ping");
            assert_eq!(second.reply_target, "octo/repo#12");
            assert_eq!(second.thread_ts.as_deref(), Some("octo/repo#12"));
            assert!(rx.recv().await.is_none());

            // Second tick: same fixtures come back from the mock, but the
            // dedup set drops them — nothing is re-forwarded.
            let (tx2, mut rx2) = tokio::sync::mpsc::channel(8);
            ch.poll_repo(&repo, &filter, &plan, &mut state, &tx2)
                .await
                .unwrap();
            drop(tx2);
            assert!(rx2.recv().await.is_none());
        }

        #[tokio::test]
        async fn sop_route_emits_sop_event_without_mention_gate() {
            use wiremock::matchers::{method, path};
            use wiremock::{Mock, MockServer, ResponseTemplate};

            let server = MockServer::start().await;
            let now = chrono::Utc::now();
            mount_token_mock(&server).await;
            // A PR opened WITHOUT mentioning the app.
            Mock::given(method("GET"))
                .and(path("/repos/octo/repo/issues"))
                .respond_with(
                    ResponseTemplate::new(200).set_body_json(serde_json::json!([{
                        "id": 556,
                        "number": 31,
                        "title": "Add events",
                        "body": "Implements the event enum.",
                        "user": {"login": "test_user", "type": "User"},
                        "created_at": (now - chrono::Duration::seconds(60)).to_rfc3339(),
                        "html_url": "https://github.com/octo/repo/pull/31",
                        "pull_request": {"merged_at": null},
                    }])),
                )
                .mount(&server)
                .await;
            mount_empty(&server, "/repos/octo/repo/issues/comments").await;

            let mut cfg = base_cfg();
            cfg.events.insert(
                "pull_request.opened".to_string(),
                zeroclaw_config::schema::GitEventRoute {
                    message: false,
                    sop: Some("pr-triage".to_string()),
                },
            );
            let plan = TransportPlan::from_routes(&cfg.events);
            let ch = mock_channel(cfg, server.uri());

            let filter = test_filter();
            let mut state = PollState::new(now - chrono::Duration::hours(1));
            let repo = RepoRef::parse("octo/repo").unwrap();
            let (tx, mut rx) = tokio::sync::mpsc::channel(8);
            ch.poll_repo(&repo, &filter, &plan, &mut state, &tx)
                .await
                .unwrap();
            drop(tx);

            // SOP-routed events bypass the mention gate and enter SOP
            // ingress with a reserved subject.
            let msg = rx.recv().await.unwrap();
            assert_eq!(msg.id, "ghpr_octo/repo#31");
            assert_eq!(msg.reply_target, "octo/repo#31");
            assert_eq!(
                msg.subject.as_deref(),
                Some("zeroclaw:sop-event:git.main:pull_request.opened")
            );
            let payload: serde_json::Value = serde_json::from_str(&msg.content).unwrap();
            assert_eq!(payload["sop"], "pr-triage");
            assert_eq!(payload["event_type"], "pull_request.opened");
            assert_eq!(payload["repo"], "octo/repo");
            assert_eq!(payload["number"], 31);
            assert_eq!(payload["body"], "Implements the event enum.");
            assert!(rx.recv().await.is_none());
        }

        #[tokio::test]
        async fn events_backbone_dedups_against_targeted_poll_and_honors_etag() {
            use wiremock::matchers::{header, method, path};
            use wiremock::{Mock, MockServer, ResponseTemplate};

            let server = MockServer::start().await;
            let now = chrono::Utc::now();
            mount_token_mock(&server).await;
            let comment = serde_json::json!({
                "id": 9001,
                "body": "@myapp ping",
                "user": {"login": "test_user", "type": "User"},
                "created_at": (now - chrono::Duration::seconds(30)).to_rfc3339(),
                "issue_url": "https://api.github.com/repos/octo/repo/issues/12",
            });
            mount_empty(&server, "/repos/octo/repo/issues").await;
            Mock::given(method("GET"))
                .and(path("/repos/octo/repo/issues/comments"))
                .respond_with(
                    ResponseTemplate::new(200).set_body_json(serde_json::json!([comment])),
                )
                .mount(&server)
                .await;
            // First feed fetch: 200 + ETag, carrying the SAME comment.
            Mock::given(method("GET"))
                .and(path("/repos/octo/repo/events"))
                .and(header("if-none-match", "W/\"feed-v1\""))
                .respond_with(ResponseTemplate::new(304))
                .expect(1)
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/repos/octo/repo/events"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .insert_header("etag", "W/\"feed-v1\"")
                        .set_body_json(serde_json::json!([{
                            "id": "36734073180",
                            "type": "IssueCommentEvent",
                            "created_at": now.to_rfc3339(),
                            "payload": {"action": "created", "comment": comment},
                        }])),
                )
                .expect(1)
                .mount(&server)
                .await;

            let cfg = GitConfig {
                events_backbone: true,
                ..base_cfg()
            };
            let ch = mock_channel(cfg, server.uri());

            let filter = test_filter();
            let plan = default_plan();
            let mut state = PollState::new(now - chrono::Duration::hours(1));
            let repo = RepoRef::parse("octo/repo").unwrap();

            // Tick 1: comment arrives via the targeted endpoint AND the
            // feed — delivered exactly once.
            let (tx, mut rx) = tokio::sync::mpsc::channel(8);
            ch.poll_repo(&repo, &filter, &plan, &mut state, &tx)
                .await
                .unwrap();
            drop(tx);
            assert_eq!(rx.recv().await.unwrap().id, "ghc_9001");
            assert!(rx.recv().await.is_none());
            assert_eq!(state.etag("octo/repo"), Some("W/\"feed-v1\""));

            // Tick 2: the stored ETag is sent and the feed answers 304
            // (the wiremock `.expect(1)` counters pin one 200 + one 304).
            let (tx2, mut rx2) = tokio::sync::mpsc::channel(8);
            ch.poll_repo(&repo, &filter, &plan, &mut state, &tx2)
                .await
                .unwrap();
            drop(tx2);
            assert!(rx2.recv().await.is_none());
        }

        #[tokio::test]
        async fn workflow_run_cursor_waits_for_pending_runs() {
            use wiremock::matchers::{method, path};
            use wiremock::{Mock, MockServer, ResponseTemplate};

            let server = MockServer::start().await;
            let now = chrono::Utc::now();
            mount_token_mock(&server).await;
            mount_empty(&server, "/repos/octo/repo/issues").await;
            mount_empty(&server, "/repos/octo/repo/issues/comments").await;
            let run = |status: &str, conclusion: Option<&str>| {
                serde_json::json!({
                    "id": 77001,
                    "name": "CI",
                    "status": status,
                    "conclusion": conclusion,
                    "created_at": (now - chrono::Duration::seconds(120)).to_rfc3339(),
                    "updated_at": now.to_rfc3339(),
                    "html_url": "https://github.com/octo/repo/actions/runs/77001",
                    "head_branch": "feat/x",
                    "run_number": 88,
                    "run_attempt": 1,
                    "actor": {"login": "test_user", "type": "User"},
                    "pull_requests": [],
                })
            };
            // Tick 1: the run is still in flight.
            Mock::given(method("GET"))
                .and(path("/repos/octo/repo/actions/runs"))
                .respond_with(ResponseTemplate::new(200).set_body_json(
                    serde_json::json!({"total_count": 1, "workflow_runs": [run("in_progress", None)]}),
                ))
                .up_to_n_times(1)
                .mount(&server)
                .await;
            // Tick 2: it completed in failure.
            Mock::given(method("GET"))
                .and(path("/repos/octo/repo/actions/runs"))
                .respond_with(ResponseTemplate::new(200).set_body_json(
                    serde_json::json!({"total_count": 1, "workflow_runs": [run("completed", Some("failure"))]}),
                ))
                .mount(&server)
                .await;

            let mut cfg = base_cfg();
            cfg.events.insert(
                "workflow_run.failed".to_string(),
                zeroclaw_config::schema::GitEventRoute {
                    message: true,
                    sop: None,
                },
            );
            let plan = TransportPlan::from_routes(&cfg.events);
            assert!(plan.workflow_runs);
            let ch = mock_channel(cfg, server.uri());

            let filter = test_filter();
            let start = now - chrono::Duration::hours(1);
            let mut state = PollState::new(start);
            let repo = RepoRef::parse("octo/repo").unwrap();

            // Tick 1: nothing surfaced; the cursor holds at the pending
            // run's creation time instead of skipping past it.
            let (tx, mut rx) = tokio::sync::mpsc::channel(8);
            ch.poll_repo(&repo, &filter, &plan, &mut state, &tx)
                .await
                .unwrap();
            drop(tx);
            assert!(rx.recv().await.is_none());
            assert_eq!(
                state.since("octo/repo", PollStream::WorkflowRuns),
                now - chrono::Duration::seconds(120)
            );

            // Tick 2: the completion is picked up.
            let (tx2, mut rx2) = tokio::sync::mpsc::channel(8);
            ch.poll_repo(&repo, &filter, &plan, &mut state, &tx2)
                .await
                .unwrap();
            drop(tx2);
            let msg = rx2.recv().await.unwrap();
            assert_eq!(msg.id, "ghwr_77001_1");
            assert!(msg.content.contains("Workflow run failed: CI #88"));
            assert!(rx2.recv().await.is_none());
        }

        #[tokio::test]
        async fn malformed_endpoint_payload_is_an_error_not_a_panic() {
            use wiremock::matchers::{method, path};
            use wiremock::{Mock, MockServer, ResponseTemplate};

            let server = MockServer::start().await;
            mount_token_mock(&server).await;
            Mock::given(method("GET"))
                .and(path("/repos/octo/repo/issues"))
                .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
                .mount(&server)
                .await;

            let ch = mock_channel(base_cfg(), server.uri());
            let filter = test_filter();
            let plan = default_plan();
            let mut state = PollState::new(chrono::Utc::now());
            let repo = RepoRef::parse("octo/repo").unwrap();
            let (tx, _rx) = tokio::sync::mpsc::channel(8);
            // The listen loop logs this and keeps polling other repos/ticks.
            let err = ch
                .poll_repo(&repo, &filter, &plan, &mut state, &tx)
                .await
                .unwrap_err();
            assert!(matches!(err, GitChannelError::Http(_)));
        }

        #[tokio::test]
        async fn draft_flow_creates_throttles_and_finalizes() {
            use wiremock::matchers::{method, path};
            use wiremock::{Mock, MockServer, ResponseTemplate};

            let server = MockServer::start().await;
            mount_token_mock(&server).await;
            Mock::given(method("POST"))
                .and(path("/repos/octo/repo/issues/5/comments"))
                .respond_with(
                    ResponseTemplate::new(201).set_body_json(serde_json::json!({"id": 42})),
                )
                .expect(1)
                .mount(&server)
                .await;
            // Exactly one PATCH: the intermediate update inside the 2 s
            // throttle window is dropped; only finalize lands.
            Mock::given(method("PATCH"))
                .and(path("/repos/octo/repo/issues/comments/42"))
                .respond_with(
                    ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": 42})),
                )
                .expect(1)
                .mount(&server)
                .await;

            let ch = mock_channel(base_cfg(), server.uri());
            let draft_id = ch
                .send_draft(&SendMessage::new("thinking…", "octo/repo#5"))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(draft_id, "42");
            ch.update_draft("octo/repo#5", "42", "partial")
                .await
                .unwrap();
            ch.finalize_draft("octo/repo#5", "42", "done", false)
                .await
                .unwrap();
        }

        #[tokio::test]
        async fn rate_limit_surfaces_reset_time() {
            use wiremock::matchers::{method, path};
            use wiremock::{Mock, MockServer, ResponseTemplate};

            let server = MockServer::start().await;
            mount_token_mock(&server).await;
            let reset = chrono::Utc::now() + chrono::Duration::seconds(120);
            Mock::given(method("GET"))
                .and(path("/repos/octo/repo/issues"))
                .respond_with(
                    ResponseTemplate::new(403)
                        .insert_header("x-ratelimit-remaining", "0")
                        .insert_header("x-ratelimit-reset", reset.timestamp().to_string().as_str()),
                )
                .mount(&server)
                .await;

            let ch = mock_channel(base_cfg(), server.uri());
            let filter = test_filter();
            let plan = default_plan();
            let mut state = PollState::new(chrono::Utc::now());
            let repo = RepoRef::parse("octo/repo").unwrap();
            let (tx, _rx) = tokio::sync::mpsc::channel(8);

            let err = ch
                .poll_repo(&repo, &filter, &plan, &mut state, &tx)
                .await
                .unwrap_err();
            match err {
                GitChannelError::RateLimited { reset_at } => {
                    assert_eq!(reset_at.timestamp(), reset.timestamp());
                }
                other => panic!("expected RateLimited, got {other:?}"),
            }
        }

        #[tokio::test]
        async fn later_stream_error_does_not_drop_earlier_events() {
            use wiremock::matchers::{method, path};
            use wiremock::{Mock, MockServer, ResponseTemplate};

            let server = MockServer::start().await;
            let now = chrono::Utc::now();
            let issue = serde_json::json!([{
                "id": 555,
                "number": 12,
                "title": "Flaky test",
                "body": "@myapp please investigate",
                "user": {"login": "test_user", "type": "User"},
                "created_at": (now - chrono::Duration::seconds(60)).to_rfc3339(),
            }]);
            mount_token_mock(&server).await;
            Mock::given(method("GET"))
                .and(path("/repos/octo/repo/issues"))
                .respond_with(ResponseTemplate::new(200).set_body_json(issue.clone()))
                .mount(&server)
                .await;
            // The Comments stream (polled after Issues) fails mid-tick.
            Mock::given(method("GET"))
                .and(path("/repos/octo/repo/issues/comments"))
                .respond_with(ResponseTemplate::new(500))
                .mount(&server)
                .await;

            let ch = mock_channel(base_cfg(), server.uri());
            let filter = test_filter();
            let plan = default_plan();
            let floor = now - chrono::Duration::hours(1);
            let mut state = PollState::new(floor);
            let repo = RepoRef::parse("octo/repo").unwrap();

            // First tick errors on the Comments stream.
            let (tx, mut rx) = tokio::sync::mpsc::channel(8);
            let err = ch
                .poll_repo(&repo, &filter, &plan, &mut state, &tx)
                .await
                .unwrap_err();
            assert!(matches!(err, GitChannelError::Api { status: 500, .. }));
            // The Issues cursor did NOT advance past the undelivered issue.
            assert_eq!(state.since(&repo.to_string(), PollStream::Issues), floor);
            drop(tx);
            assert!(rx.recv().await.is_none());

            // Fix the Comments stream and poll again: the issue is still
            // delivered (it was never marked seen), proving no event was lost.
            server.reset().await;
            mount_token_mock(&server).await;
            Mock::given(method("GET"))
                .and(path("/repos/octo/repo/issues"))
                .respond_with(ResponseTemplate::new(200).set_body_json(issue))
                .mount(&server)
                .await;
            mount_empty(&server, "/repos/octo/repo/issues/comments").await;

            let (tx2, mut rx2) = tokio::sync::mpsc::channel(8);
            ch.poll_repo(&repo, &filter, &plan, &mut state, &tx2)
                .await
                .unwrap();
            drop(tx2);
            let msg = rx2.recv().await.unwrap();
            assert_eq!(msg.id, "ghi_555");
            assert!(rx2.recv().await.is_none());
        }

        #[tokio::test]
        async fn issues_stream_follows_link_pagination() {
            use wiremock::matchers::{method, path};
            use wiremock::{Mock, MockServer, ResponseTemplate};

            let server = MockServer::start().await;
            let now = chrono::Utc::now();
            mount_token_mock(&server).await;
            // Page one points at page two the way GitHub's Link header does.
            let next_link = format!("<{}/paged/issues/2>; rel=\"next\"", server.uri());
            Mock::given(method("GET"))
                .and(path("/repos/octo/repo/issues"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .insert_header("link", next_link.as_str())
                        .set_body_json(serde_json::json!([{
                            "id": 101,
                            "number": 1,
                            "title": "First",
                            "body": "@myapp one",
                            "user": {"login": "test_user", "type": "User"},
                            "created_at": (now - chrono::Duration::seconds(90)).to_rfc3339(),
                        }])),
                )
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/paged/issues/2"))
                .respond_with(
                    ResponseTemplate::new(200).set_body_json(serde_json::json!([{
                        "id": 202,
                        "number": 2,
                        "title": "Second",
                        "body": "@myapp two",
                        "user": {"login": "test_user", "type": "User"},
                        "created_at": (now - chrono::Duration::seconds(30)).to_rfc3339(),
                    }])),
                )
                .mount(&server)
                .await;
            mount_empty(&server, "/repos/octo/repo/issues/comments").await;

            let ch = mock_channel(base_cfg(), server.uri());
            let filter = test_filter();
            let plan = default_plan();
            let mut state = PollState::new(now - chrono::Duration::hours(1));
            let repo = RepoRef::parse("octo/repo").unwrap();
            let (tx, mut rx) = tokio::sync::mpsc::channel(8);
            ch.poll_repo(&repo, &filter, &plan, &mut state, &tx)
                .await
                .unwrap();
            drop(tx);

            // Both pages' issues are delivered, oldest first.
            let first = rx.recv().await.unwrap();
            assert_eq!(first.id, "ghi_101");
            let second = rx.recv().await.unwrap();
            assert_eq!(second.id, "ghi_202");
            assert!(rx.recv().await.is_none());
        }
    }
}
