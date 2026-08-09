//! `GitProvider` — the git-forge provider seam.

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use super::events::GitEvent;
use super::poll::PollStream;
use super::types::{GitChannelError, IssueRef, RepoRef};

/// The bot's own identity on the forge.
pub struct SelfIdentity {
    /// The handle users type to address the bot (e.g. a GitHub App slug,
    /// mentioned as `@<slug>`).
    pub mention_handle: String,
    /// The author login the bot posts under (e.g. `<slug>[bot]`), used to
    /// drop the bot's own activity from the inbound stream.
    pub bot_login: String,
}

pub struct FetchPage {
    /// Normalized events surfaced this fetch (pre-dedup; the core dedups).
    pub events: Vec<GitEvent>,
    /// Advance the stream cursor to this instant. `None` leaves the cursor
    /// unchanged (e.g. a 304 not-modified feed, or a stream that advances
    /// only on items the core has not yet seen).
    pub advance_to: Option<DateTime<Utc>>,
    /// New ETag to store for the feed stream (conditional-request backbone).
    pub etag: Option<String>,
    /// The feed returned 304 Not Modified — nothing changed since `etag`.
    pub not_modified: bool,
}

/// What a reaction targets on the forge.
pub enum ReactionTarget {
    /// A reaction on a specific comment.
    Comment {
        repo: RepoRef,
        /// Provider-native comment id (string at the boundary; providers
        /// convert to/from their native id type).
        comment_id: String,
    },
    /// A reaction on the issue/PR itself.
    Issue(IssueRef),
}

/// A git forge the channel can converse through. One implementation per forge.
/// Providers MUST be cheap to hold and internally cache their own auth/tokens;
/// the core calls these per poll tick and per outbound message.
#[async_trait]
pub trait GitProvider: Send + Sync {
    /// Stable forge name for logs and attribution (e.g. `"github"`).
    fn name(&self) -> &'static str;

    /// Resolve the bot's own identity (mention handle + bot login). Cached by
    /// the provider after the first call.
    async fn self_identity(&self) -> Result<SelfIdentity, GitChannelError>;

    /// Repositories to poll when config does not enumerate them — i.e. those
    /// visible to the configured credential/installation.
    async fn discover_repos(&self) -> Result<Vec<RepoRef>, GitChannelError>;

    /// Fetch new events for one `repo`+`stream` since `since` (an event-time
    /// cursor; `etag` carries the feed stream's conditional-request token).
    /// The provider normalizes its native payloads into [`GitEvent`]s and
    /// reports the cursor to advance to.
    async fn fetch(
        &self,
        repo: &RepoRef,
        stream: PollStream,
        since: DateTime<Utc>,
        etag: Option<&str>,
    ) -> Result<FetchPage, GitChannelError>;

    /// Post a comment on the issue/PR; returns the new comment's id.
    async fn post_comment(&self, target: &IssueRef, body: &str) -> Result<String, GitChannelError>;

    /// Replace an existing comment's body (draft streaming).
    async fn edit_comment(
        &self,
        repo: &RepoRef,
        comment_id: &str,
        body: &str,
    ) -> Result<(), GitChannelError>;

    /// Delete a comment (draft cancellation).
    async fn delete_comment(&self, repo: &RepoRef, comment_id: &str)
    -> Result<(), GitChannelError>;

    /// Add a reaction. `emoji` is the channel-neutral name; the provider maps
    /// it onto its own reaction set and silently drops unmappable ones
    /// (matching the `Channel` trait's best-effort reaction contract).
    async fn add_reaction(
        &self,
        target: &ReactionTarget,
        emoji: &str,
    ) -> Result<(), GitChannelError>;

    /// Low-level, provider-relative forge API call. The transport seam every
    /// higher-level forge operation is built on: the provider prepends its API
    /// base, attaches auth, sends `req`, and returns the status plus decoded
    /// JSON body without raising on non-2xx (the caller inspects the forge's
    /// own error envelope). This is what lets the `git_forge` tool carry a
    /// resource/action vocabulary and a `raw` catch-all over one seam, without
    /// a new trait method per operation.
    async fn forge_request(
        &self,
        req: super::types::ForgeRequest,
    ) -> Result<super::types::ForgeResponse, GitChannelError>;
}
