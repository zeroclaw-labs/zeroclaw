//! Provider-agnostic git-forge events and their mapping to
//! `ChannelMessage`s.

use chrono::{DateTime, Utc};
use serde_json::json;
use zeroclaw_api::channel::{CHANNEL_SOP_SUBJECT_PREFIX, ChannelMessage};

use super::types::{
    EVT_ISSUE_COMMENT_CREATED, EVT_ISSUES_OPENED, EVT_PR_REVIEW_COMMENT_CREATED,
    EVT_PULL_REQUEST_CLOSED, EVT_PULL_REQUEST_MERGED, EVT_PULL_REQUEST_OPENED,
    EVT_RELEASE_PUBLISHED, EVT_WORKFLOW_RUN_COMPLETED, EVT_WORKFLOW_RUN_FAILED, IssueRef, RepoRef,
};

/// The accountable actor behind an event, normalized across forges: the
/// login used for allowlisting and self/bot filtering, plus whether the
/// account is a bot. Providers map their native user payloads onto this.
#[derive(Debug, Clone)]
pub struct EventActor {
    /// Account login (`test_user`, `dependabot[bot]`, …). Compared
    /// case-insensitively against the bot login and the allowlist.
    pub login: String,
    /// Whether the account is a bot/service identity.
    pub is_bot: bool,
}

/// Inbound filtering parameters, derived from channel config and the
/// bot identity resolved at listen start.
pub struct EventFilter<'a> {
    /// The bot's own login (e.g. `<slug>[bot]`) — its own events are
    /// always dropped.
    pub bot_login: &'a str,
    /// The handle users type to address the bot (`@<handle>`, without `@`).
    pub mention_handle: &'a str,
    pub mention_only: bool,
    pub listen_to_bots: bool,
}

impl EventFilter<'_> {
    /// Self/bot author gate — applies to every event regardless of route.
    fn admit_author(&self, author: &EventActor) -> bool {
        if author.login.eq_ignore_ascii_case(self.bot_login) {
            return false;
        }
        !author.is_bot || self.listen_to_bots
    }

    /// Text gate for user-authored bodies: when `gated`, require the
    /// mention (under `mention_only`); always strip it and tidy
    /// whitespace. `None` when the gate fails or nothing remains.
    fn admit_text(&self, body: &str, gated: bool) -> Option<String> {
        if gated && self.mention_only && !contains_mention(body, self.mention_handle) {
            return None;
        }
        let content = strip_mention(body, self.mention_handle);
        if content.is_empty() {
            return None;
        }
        Some(content)
    }
}

// ── Normalized event shapes ─────────────────────────────────────────

/// A new comment (issue/PR comment or inline review comment).
#[derive(Debug, Clone)]
pub struct CommentPost {
    pub repo: RepoRef,
    pub number: u64,
    /// Provider-native comment id, stringified at the boundary.
    pub comment_id: String,
    pub author: EventActor,
    pub body: String,
    pub created_at: DateTime<Utc>,
}

/// The opening post of a new issue.
#[derive(Debug, Clone)]
pub struct IssuePost {
    pub repo: RepoRef,
    pub number: u64,
    /// Provider-native issue id, stringified at the boundary.
    pub issue_id: String,
    pub author: EventActor,
    pub title: String,
    pub body: String,
    pub created_at: DateTime<Utc>,
}

/// The opening post of a new pull request. Unlike issues, PRs may carry no
/// transport-stable object id (GitHub's issue-view and pull-view id spaces
/// differ), so identity keys on `owner/repo#number`.
#[derive(Debug, Clone)]
pub struct PullPost {
    pub repo: RepoRef,
    pub number: u64,
    pub author: EventActor,
    pub title: String,
    pub body: String,
    pub created_at: DateTime<Utc>,
}

/// A pull request leaving the open state (closed or merged). `author`
/// is the PR's author — the polling transports don't say who closed it.
#[derive(Debug, Clone)]
pub struct PullTransition {
    pub repo: RepoRef,
    pub number: u64,
    pub title: String,
    pub author: EventActor,
    pub html_url: String,
    pub at: DateTime<Utc>,
}

/// A finished workflow/pipeline run.
#[derive(Debug, Clone)]
pub struct RunOutcome {
    pub repo: RepoRef,
    /// Provider-native run id, stringified at the boundary.
    pub run_id: String,
    pub attempt: u64,
    pub name: String,
    pub branch: Option<String>,
    pub run_number: u64,
    /// First associated PR, when the forge reports one (GitHub doesn't for
    /// fork-triggered runs). Used as the reply surface.
    pub pr_number: Option<u64>,
    pub actor: Option<EventActor>,
    pub html_url: String,
    pub finished_at: DateTime<Utc>,
}

/// A published release.
#[derive(Debug, Clone)]
pub struct ReleasePost {
    pub repo: RepoRef,
    /// Provider-native release id, stringified at the boundary.
    pub release_id: String,
    pub tag: String,
    pub name: Option<String>,
    pub author: EventActor,
    pub body: String,
    pub html_url: String,
    pub published_at: DateTime<Utc>,
}

/// A normalized git-forge event, independent of provider and transport.
/// Variants carry only the fields routing and message mapping need.
#[derive(Debug, Clone)]
pub enum GitEvent {
    /// `issue_comment.created` — new comment on an issue or PR.
    IssueCommentCreated(CommentPost),
    /// `issues.opened` — a new issue's opening post.
    IssueOpened(IssuePost),
    /// `pull_request.opened` — a new PR's opening post.
    PullRequestOpened(PullPost),
    /// `pull_request.closed` — closed without merging.
    PullRequestClosed(PullTransition),
    /// `pull_request.merged`.
    PullRequestMerged(PullTransition),
    /// `pull_request_review_comment.created` — inline diff comment.
    PullRequestReviewCommentCreated(CommentPost),
    /// `workflow_run.completed` — run finished successfully.
    WorkflowRunCompleted(RunOutcome),
    /// `workflow_run.failed` — run finished in failure.
    WorkflowRunFailed(RunOutcome),
    /// `release.published`.
    ReleasePublished(ReleasePost),
}

impl GitEvent {
    /// The routing-table key for this event.
    pub fn event_type(&self) -> &'static str {
        match self {
            Self::IssueCommentCreated(_) => EVT_ISSUE_COMMENT_CREATED,
            Self::IssueOpened(_) => EVT_ISSUES_OPENED,
            Self::PullRequestOpened(_) => EVT_PULL_REQUEST_OPENED,
            Self::PullRequestClosed(_) => EVT_PULL_REQUEST_CLOSED,
            Self::PullRequestMerged(_) => EVT_PULL_REQUEST_MERGED,
            Self::PullRequestReviewCommentCreated(_) => EVT_PR_REVIEW_COMMENT_CREATED,
            Self::WorkflowRunCompleted(_) => EVT_WORKFLOW_RUN_COMPLETED,
            Self::WorkflowRunFailed(_) => EVT_WORKFLOW_RUN_FAILED,
            Self::ReleasePublished(_) => EVT_RELEASE_PUBLISHED,
        }
    }

    pub fn dedup_id(&self) -> String {
        match self {
            Self::IssueCommentCreated(p) => format!("ghc_{}", p.comment_id),
            Self::IssueOpened(p) => format!("ghi_{}", p.issue_id),
            Self::PullRequestOpened(p) => format!("ghpr_{}#{}", p.repo, p.number),
            Self::PullRequestClosed(t) | Self::PullRequestMerged(t) => {
                format!("ghprx_{}#{}@{}", t.repo, t.number, t.at.timestamp())
            }
            Self::PullRequestReviewCommentCreated(p) => format!("ghrc_{}", p.comment_id),
            Self::WorkflowRunCompleted(r) | Self::WorkflowRunFailed(r) => {
                format!("ghwr_{}_{}", r.run_id, r.attempt)
            }
            Self::ReleasePublished(r) => format!("ghrel_{}", r.release_id),
        }
    }

    /// When the event happened (creation, close, completion, publish).
    pub fn created_at(&self) -> DateTime<Utc> {
        match self {
            Self::IssueCommentCreated(p) | Self::PullRequestReviewCommentCreated(p) => p.created_at,
            Self::IssueOpened(p) => p.created_at,
            Self::PullRequestOpened(p) => p.created_at,
            Self::PullRequestClosed(t) | Self::PullRequestMerged(t) => t.at,
            Self::WorkflowRunCompleted(r) | Self::WorkflowRunFailed(r) => r.finished_at,
            Self::ReleasePublished(r) => r.published_at,
        }
    }

    /// The accountable user: comment/post author, PR author, run actor,
    /// release author. `None` only for runs missing an actor.
    pub fn author(&self) -> Option<&EventActor> {
        match self {
            Self::IssueCommentCreated(p) | Self::PullRequestReviewCommentCreated(p) => {
                Some(&p.author)
            }
            Self::IssueOpened(p) => Some(&p.author),
            Self::PullRequestOpened(p) => Some(&p.author),
            Self::PullRequestClosed(t) | Self::PullRequestMerged(t) => Some(&t.author),
            Self::WorkflowRunCompleted(r) | Self::WorkflowRunFailed(r) => r.actor.as_ref(),
            Self::ReleasePublished(r) => Some(&r.author),
        }
    }

    /// Whether the event is user-authored conversation (where mention
    /// gating is meaningful) as opposed to a lifecycle/CI notification.
    pub fn is_conversational(&self) -> bool {
        matches!(
            self,
            Self::IssueCommentCreated(_)
                | Self::IssueOpened(_)
                | Self::PullRequestOpened(_)
                | Self::PullRequestReviewCommentCreated(_)
        )
    }
}

pub fn event_to_message(
    event: &GitEvent,
    filter: &EventFilter<'_>,
    channel_key: &str,
    alias: &str,
    gate_mentions: bool,
) -> Option<ChannelMessage> {
    // Authorless events (a run with no actor) carry no identity to gate
    // on — drop them rather than bypass the author filters.
    let author = event.author()?;
    if !filter.admit_author(author) {
        return None;
    }
    let gated = gate_mentions && event.is_conversational();
    let id = event.dedup_id();
    let timestamp = event.created_at().timestamp();
    let sender = author.login.clone();
    let ctx = MessageCtx { channel_key, alias };

    match event {
        GitEvent::IssueCommentCreated(p) | GitEvent::PullRequestReviewCommentCreated(p) => {
            let content = filter.admit_text(&p.body, gated)?;
            let target = issue_target(&p.repo, p.number);
            Some(message(id, sender, &target, content, timestamp, None, &ctx))
        }
        GitEvent::IssueOpened(p) => {
            let content = opening_content(filter, &p.body, gated, || {
                format!("Issue #{} opened: {}", p.number, p.title)
            })?;
            let target = issue_target(&p.repo, p.number);
            let subject = Some(p.title.clone());
            Some(message(
                id, sender, &target, content, timestamp, subject, &ctx,
            ))
        }
        GitEvent::PullRequestOpened(p) => {
            let content = opening_content(filter, &p.body, gated, || {
                format!("Pull request #{} opened: {}", p.number, p.title)
            })?;
            let target = issue_target(&p.repo, p.number);
            let subject = Some(p.title.clone());
            Some(message(
                id, sender, &target, content, timestamp, subject, &ctx,
            ))
        }
        GitEvent::PullRequestClosed(t) | GitEvent::PullRequestMerged(t) => {
            let verb = if matches!(event, GitEvent::PullRequestMerged(_)) {
                "merged"
            } else {
                "closed without merging"
            };
            let mut content = format!(
                "Pull request {}#{} ({}) was {verb}.",
                t.repo, t.number, t.title
            );
            if !t.html_url.is_empty() {
                content.push_str(&format!("\n{}", t.html_url));
            }
            let target = issue_target(&t.repo, t.number);
            let subject = Some(t.title.clone());
            Some(message(
                id, sender, &target, content, timestamp, subject, &ctx,
            ))
        }
        GitEvent::WorkflowRunCompleted(r) | GitEvent::WorkflowRunFailed(r) => {
            let verdict = if matches!(event, GitEvent::WorkflowRunFailed(_)) {
                "failed"
            } else {
                "completed successfully"
            };
            let mut content = format!("Workflow run {verdict}: {} #{}", r.name, r.run_number);
            if let Some(branch) = &r.branch {
                content.push_str(&format!(" on {branch}"));
            }
            if r.attempt > 1 {
                content.push_str(&format!(" (attempt {})", r.attempt));
            }
            if !r.html_url.is_empty() {
                content.push_str(&format!("\n{}", r.html_url));
            }
            // Replies need an issue/PR context; without an associated PR
            // the target is the bare repo and replies will be rejected.
            let target = match r.pr_number {
                Some(n) => issue_target(&r.repo, n),
                None => r.repo.to_string(),
            };
            let subject = Some(r.name.clone());
            Some(message(
                id, sender, &target, content, timestamp, subject, &ctx,
            ))
        }
        GitEvent::ReleasePublished(r) => {
            let mut content = format!("Release {} published", r.tag);
            if !r.html_url.is_empty() {
                content.push_str(&format!(": {}", r.html_url));
            }
            if !r.body.trim().is_empty() {
                content.push_str(&format!("\n\n{}", r.body.trim()));
            }
            let subject = Some(format!(
                "Release {}",
                r.name
                    .clone()
                    .filter(|n| !n.is_empty())
                    .unwrap_or_else(|| r.tag.clone())
            ));
            Some(message(
                id,
                sender,
                &r.repo.to_string(),
                content,
                timestamp,
                subject,
                &ctx,
            ))
        }
    }
}

/// Map a routed git event into a channel-carried SOP event. The message
/// envelope exists only to reuse the channel listener bus; the orchestrator
/// consumes it before debounce, `/stop`, or LLM processing.
pub fn event_to_sop_message(
    event: &GitEvent,
    filter: &EventFilter<'_>,
    channel_key: &str,
    alias: &str,
    provider: &str,
    sop: &str,
) -> Option<ChannelMessage> {
    // Authorless events carry no identity to gate on; keep that behavior
    // aligned with conversational messages.
    let author = event.author()?;
    if !filter.admit_author(author) {
        return None;
    }

    let id = event.dedup_id();
    let timestamp = event.created_at().timestamp();
    let target = event_target(event);
    let topic = sop_topic(channel_key, alias, event.event_type());
    let payload = event_payload(event, channel_key, alias, provider, sop, &topic);
    let ctx = MessageCtx { channel_key, alias };
    let mut msg = message(
        id,
        author.login.clone(),
        &target,
        payload.to_string(),
        timestamp,
        Some(format!("{CHANNEL_SOP_SUBJECT_PREFIX}{topic}")),
        &ctx,
    );
    // Internal marker the orchestrator keys SOP routing on. Only this git
    // producer sets it, so an inbound user/email message cannot forge a SOP
    // event by crafting its `subject`. The subject above stays human-readable
    // for logs and reply threading but is no longer the routing trigger.
    msg.internal_sop_event = Some(topic);
    Some(msg)
}

fn sop_topic(channel_key: &str, alias: &str, event_type: &str) -> String {
    zeroclaw_api::channel::ChannelSopTopic::build(channel_key, alias, event_type)
}

fn event_target(event: &GitEvent) -> String {
    match event {
        GitEvent::IssueCommentCreated(p) | GitEvent::PullRequestReviewCommentCreated(p) => {
            issue_target(&p.repo, p.number)
        }
        GitEvent::IssueOpened(p) => issue_target(&p.repo, p.number),
        GitEvent::PullRequestOpened(p) => issue_target(&p.repo, p.number),
        GitEvent::PullRequestClosed(t) | GitEvent::PullRequestMerged(t) => {
            issue_target(&t.repo, t.number)
        }
        GitEvent::WorkflowRunCompleted(r) | GitEvent::WorkflowRunFailed(r) => match r.pr_number {
            Some(number) => issue_target(&r.repo, number),
            None => r.repo.to_string(),
        },
        GitEvent::ReleasePublished(r) => r.repo.to_string(),
    }
}

fn actor_payload(actor: &EventActor) -> serde_json::Value {
    json!({
        "login": actor.login,
        "is_bot": actor.is_bot,
    })
}

fn common_payload(
    event: &GitEvent,
    channel_key: &str,
    alias: &str,
    provider: &str,
    sop: &str,
    topic: &str,
) -> serde_json::Value {
    json!({
        "source": "channel",
        "channel": channel_key,
        "channel_alias": alias,
        "provider": provider,
        "sop": sop,
        "topic": topic,
        "event_type": event.event_type(),
        "dedup_id": event.dedup_id(),
        "created_at": event.created_at().to_rfc3339(),
        "target": event_target(event),
    })
}

fn event_payload(
    event: &GitEvent,
    channel_key: &str,
    alias: &str,
    provider: &str,
    sop: &str,
    topic: &str,
) -> serde_json::Value {
    let mut payload = common_payload(event, channel_key, alias, provider, sop, topic);
    let obj = payload
        .as_object_mut()
        .expect("common_payload produces a JSON object");
    match event {
        GitEvent::IssueCommentCreated(p) | GitEvent::PullRequestReviewCommentCreated(p) => {
            obj.insert("repo".into(), json!(p.repo.to_string()));
            obj.insert("number".into(), json!(p.number));
            obj.insert("comment_id".into(), json!(p.comment_id));
            obj.insert("author".into(), actor_payload(&p.author));
            obj.insert("body".into(), json!(p.body));
        }
        GitEvent::IssueOpened(p) => {
            obj.insert("repo".into(), json!(p.repo.to_string()));
            obj.insert("number".into(), json!(p.number));
            obj.insert("issue_id".into(), json!(p.issue_id));
            obj.insert("author".into(), actor_payload(&p.author));
            obj.insert("title".into(), json!(p.title));
            obj.insert("body".into(), json!(p.body));
        }
        GitEvent::PullRequestOpened(p) => {
            obj.insert("repo".into(), json!(p.repo.to_string()));
            obj.insert("number".into(), json!(p.number));
            obj.insert("author".into(), actor_payload(&p.author));
            obj.insert("title".into(), json!(p.title));
            obj.insert("body".into(), json!(p.body));
        }
        GitEvent::PullRequestClosed(t) | GitEvent::PullRequestMerged(t) => {
            obj.insert("repo".into(), json!(t.repo.to_string()));
            obj.insert("number".into(), json!(t.number));
            obj.insert("author".into(), actor_payload(&t.author));
            obj.insert("title".into(), json!(t.title));
            obj.insert("html_url".into(), json!(t.html_url));
        }
        GitEvent::WorkflowRunCompleted(r) | GitEvent::WorkflowRunFailed(r) => {
            obj.insert("repo".into(), json!(r.repo.to_string()));
            obj.insert("run_id".into(), json!(r.run_id));
            obj.insert("attempt".into(), json!(r.attempt));
            obj.insert("name".into(), json!(r.name));
            obj.insert("branch".into(), json!(r.branch));
            obj.insert("run_number".into(), json!(r.run_number));
            obj.insert("pr_number".into(), json!(r.pr_number));
            obj.insert("actor".into(), r.actor.as_ref().map(actor_payload).into());
            obj.insert("html_url".into(), json!(r.html_url));
        }
        GitEvent::ReleasePublished(r) => {
            obj.insert("repo".into(), json!(r.repo.to_string()));
            obj.insert("release_id".into(), json!(r.release_id));
            obj.insert("tag".into(), json!(r.tag));
            obj.insert("name".into(), json!(r.name));
            obj.insert("author".into(), actor_payload(&r.author));
            obj.insert("body".into(), json!(r.body));
            obj.insert("html_url".into(), json!(r.html_url));
        }
    }
    payload
}

/// Per-message attribution context: the channel key and configured alias.
struct MessageCtx<'a> {
    channel_key: &'a str,
    alias: &'a str,
}

/// Content for an opening post: the gated body on the message path; on
/// ungated (sop-routed) paths an empty body falls back to a synthesized
/// line so the event still carries the title.
fn opening_content(
    filter: &EventFilter<'_>,
    body: &str,
    gated: bool,
    fallback: impl FnOnce() -> String,
) -> Option<String> {
    match filter.admit_text(body, gated) {
        Some(content) => Some(content),
        None if !gated => Some(fallback()),
        None => None,
    }
}

fn issue_target(repo: &RepoRef, number: u64) -> String {
    IssueRef {
        repo: repo.clone(),
        number,
    }
    .to_string()
}

fn message(
    id: String,
    sender: String,
    target: &str,
    content: String,
    timestamp: i64,
    subject: Option<String>,
    ctx: &MessageCtx<'_>,
) -> ChannelMessage {
    ChannelMessage {
        id,
        sender,
        reply_target: target.to_string(),
        content,
        channel: ctx.channel_key.to_string(),
        channel_alias: Some(ctx.alias.to_string()),
        timestamp: timestamp.max(0) as u64,
        // Conversation context is target-scoped: every message on the
        // same issue/PR (or repo, for repo-level events) shares a thread.
        thread_ts: Some(target.to_string()),
        subject,
        ..ChannelMessage::default()
    }
}

pub fn contains_mention(body: &str, handle: &str) -> bool {
    if handle.is_empty() {
        return false;
    }
    let body_lower = body.to_ascii_lowercase();
    let needle = format!("@{}", handle.to_ascii_lowercase());
    let mut start = 0;
    while let Some(pos) = body_lower[start..].find(&needle) {
        let end = start + pos + needle.len();
        let boundary = body_lower[end..]
            .chars()
            .next()
            .is_none_or(|c| !(c.is_alphanumeric() || c == '-' || c == '_'));
        if boundary {
            return true;
        }
        start = end;
    }
    false
}

/// Remove every word-boundary `@handle` mention and tidy whitespace.
pub fn strip_mention(body: &str, handle: &str) -> String {
    if handle.is_empty() {
        return body.trim().to_string();
    }
    let needle = format!("@{}", handle.to_ascii_lowercase());
    let mut out = String::with_capacity(body.len());
    let body_lower = body.to_ascii_lowercase();
    let mut idx = 0;
    while let Some(pos) = body_lower[idx..].find(&needle) {
        let abs = idx + pos;
        let end = abs + needle.len();
        let boundary = body_lower[end..]
            .chars()
            .next()
            .is_none_or(|c| !(c.is_alphanumeric() || c == '-' || c == '_'));
        out.push_str(&body[idx..abs]);
        if !boundary {
            out.push_str(&body[abs..end]);
        }
        idx = end;
    }
    out.push_str(&body[idx..]);
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filter(mention_only: bool, listen_to_bots: bool) -> EventFilter<'static> {
        EventFilter {
            bot_login: "myapp[bot]",
            mention_handle: "myapp",
            mention_only,
            listen_to_bots,
        }
    }

    fn repo() -> RepoRef {
        RepoRef::parse("octo/repo").unwrap()
    }

    fn actor(login: &str, is_bot: bool) -> EventActor {
        EventActor {
            login: login.to_string(),
            is_bot,
        }
    }

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    /// Map an event through the generic channel context (key `git`).
    fn to_message(
        event: &GitEvent,
        filter: &EventFilter<'_>,
        gate: bool,
    ) -> Option<ChannelMessage> {
        event_to_message(event, filter, "git", "main", gate)
    }

    fn comment_event(login: &str, is_bot: bool, body: &str, number: u64) -> GitEvent {
        GitEvent::IssueCommentCreated(CommentPost {
            repo: repo(),
            number,
            comment_id: "9001".to_string(),
            author: actor(login, is_bot),
            body: body.to_string(),
            created_at: at("2026-06-13T01:02:03Z"),
        })
    }

    #[test]
    fn issue_comment_maps_with_issue_threading() {
        let event = comment_event("test_user", false, "@myapp run the tests", 7);
        assert_eq!(event.event_type(), "issue_comment.created");
        assert_eq!(event.dedup_id(), "ghc_9001");
        assert!(event.is_conversational());

        let msg = to_message(&event, &filter(true, false), true).unwrap();
        assert_eq!(msg.id, "ghc_9001");
        assert_eq!(msg.sender, "test_user");
        assert_eq!(msg.reply_target, "octo/repo#7");
        assert_eq!(msg.thread_ts.as_deref(), Some("octo/repo#7"));
        assert_eq!(msg.content, "run the tests");
        assert_eq!(msg.channel, "git");
        assert_eq!(msg.channel_alias.as_deref(), Some("main"));
    }

    #[test]
    fn opening_posts_carry_subject_and_pr_identity() {
        let issue = GitEvent::IssueOpened(IssuePost {
            repo: repo(),
            number: 12,
            issue_id: "555".to_string(),
            author: actor("test_user", false),
            title: "Flaky test".to_string(),
            body: "@myapp please investigate".to_string(),
            created_at: at("2026-06-13T01:00:00Z"),
        });
        assert_eq!(issue.event_type(), "issues.opened");
        assert_eq!(issue.dedup_id(), "ghi_555");
        let msg = to_message(&issue, &filter(true, false), true).unwrap();
        assert_eq!(msg.id, "ghi_555");
        assert_eq!(msg.reply_target, "octo/repo#12");
        assert_eq!(msg.subject.as_deref(), Some("Flaky test"));
        assert_eq!(msg.content, "please investigate");

        let pr = GitEvent::PullRequestOpened(PullPost {
            repo: repo(),
            number: 12,
            author: actor("test_user", false),
            title: "Flaky test".to_string(),
            body: "x".to_string(),
            created_at: at("2026-06-13T01:00:00Z"),
        });
        assert_eq!(pr.event_type(), "pull_request.opened");
        // PRs have no transport-stable object id; identity is repo#number.
        assert_eq!(pr.dedup_id(), "ghpr_octo/repo#12");
    }

    #[test]
    fn sop_message_carries_reserved_subject_and_structured_payload() {
        let event = GitEvent::PullRequestOpened(PullPost {
            repo: repo(),
            number: 12,
            author: actor("test_user", false),
            title: "Route through SOP".to_string(),
            body: "Please review".to_string(),
            created_at: at("2026-06-13T01:00:00Z"),
        });

        let msg = event_to_sop_message(
            &event,
            &filter(true, false),
            "git",
            "main",
            "github",
            "triage",
        )
        .unwrap();
        assert_eq!(
            msg.subject.as_deref(),
            Some("zeroclaw:sop-event:git.main:pull_request.opened")
        );
        assert_eq!(msg.reply_target, "octo/repo#12");

        let payload: serde_json::Value = serde_json::from_str(&msg.content).unwrap();
        assert_eq!(payload["source"], "channel");
        assert_eq!(payload["channel"], "git");
        assert_eq!(payload["channel_alias"], "main");
        assert_eq!(payload["provider"], "github");
        assert_eq!(payload["sop"], "triage");
        assert_eq!(payload["topic"], "git.main:pull_request.opened");
        assert_eq!(payload["event_type"], "pull_request.opened");
        assert_eq!(payload["repo"], "octo/repo");
        assert_eq!(payload["number"], 12);
        assert_eq!(payload["author"]["login"], "test_user");
        assert_eq!(payload["title"], "Route through SOP");
    }

    #[test]
    fn pull_transition_message_shape() {
        let merged = GitEvent::PullRequestMerged(PullTransition {
            repo: repo(),
            number: 12,
            title: "Flaky test".to_string(),
            author: actor("test_user", false),
            html_url: "https://github.com/octo/repo/issues/12".to_string(),
            at: at("2026-06-13T02:00:00Z"),
        });
        assert_eq!(merged.event_type(), "pull_request.merged");
        let msg = to_message(&merged, &filter(true, false), true).unwrap();
        assert!(msg.content.contains("was merged"));
        assert!(msg.content.contains("octo/repo#12"));
        assert!(
            msg.content
                .contains("https://github.com/octo/repo/issues/12")
        );
        assert_eq!(msg.reply_target, "octo/repo#12");
    }

    #[test]
    fn workflow_run_message_targets_associated_pr_or_bare_repo() {
        let with_pr = GitEvent::WorkflowRunFailed(RunOutcome {
            repo: repo(),
            run_id: "77001".to_string(),
            attempt: 1,
            name: "CI".to_string(),
            branch: Some("feat/x".to_string()),
            run_number: 88,
            pr_number: Some(31),
            actor: Some(actor("test_user", false)),
            html_url: "https://github.com/octo/repo/actions/runs/77001".to_string(),
            finished_at: at("2026-06-13T01:14:00Z"),
        });
        assert_eq!(with_pr.dedup_id(), "ghwr_77001_1");
        assert!(!with_pr.is_conversational());
        // Non-conversational events bypass the mention gate.
        let msg = to_message(&with_pr, &filter(true, false), true).unwrap();
        assert_eq!(msg.sender, "test_user");
        assert_eq!(msg.reply_target, "octo/repo#31");
        assert_eq!(msg.subject.as_deref(), Some("CI"));
        assert!(
            msg.content
                .contains("Workflow run failed: CI #88 on feat/x")
        );
        assert!(msg.content.contains("actions/runs/77001"));

        let mut bare = with_pr.clone();
        if let GitEvent::WorkflowRunFailed(r) = &mut bare {
            r.pr_number = None;
        }
        let msg = to_message(&bare, &filter(true, false), true).unwrap();
        assert_eq!(msg.reply_target, "octo/repo");
    }

    #[test]
    fn release_message_shape() {
        let event = GitEvent::ReleasePublished(ReleasePost {
            repo: repo(),
            release_id: "6100".to_string(),
            tag: "v0.9.0".to_string(),
            name: Some("v0.9.0 — events".to_string()),
            author: actor("test_user", false),
            body: "Changelog body".to_string(),
            html_url: "https://github.com/octo/repo/releases/tag/v0.9.0".to_string(),
            published_at: at("2026-06-13T01:20:00Z"),
        });
        assert_eq!(event.event_type(), "release.published");
        assert_eq!(event.dedup_id(), "ghrel_6100");
        let msg = to_message(&event, &filter(true, false), true).unwrap();
        assert_eq!(msg.subject.as_deref(), Some("Release v0.9.0 — events"));
        assert!(msg.content.starts_with("Release v0.9.0 published"));
        assert!(msg.content.contains("Changelog body"));
        assert_eq!(msg.reply_target, "octo/repo");
    }

    // ── Filters: self/bot/mention across routes ─────────────────────

    #[test]
    fn unmentioned_comment_dropped_only_on_the_gated_path() {
        let event = comment_event("test_user", false, "just chatting", 7);
        // Message path under mention_only: dropped.
        assert!(to_message(&event, &filter(true, false), true).is_none());
        // mention_only off: accepted.
        assert!(to_message(&event, &filter(false, false), true).is_some());
        // Sop-routed delivery (gate off): accepted despite mention_only.
        assert!(to_message(&event, &filter(true, false), false).is_some());
    }

    #[test]
    fn own_bot_events_always_dropped_even_ungated() {
        let event = comment_event("myapp[bot]", true, "@myapp echo", 7);
        assert!(to_message(&event, &filter(false, true), false).is_none());

        // …including non-conversational events authored by ourselves.
        let release = GitEvent::ReleasePublished(ReleasePost {
            repo: repo(),
            release_id: "6100".to_string(),
            tag: "v0.9.0".to_string(),
            name: None,
            author: actor("myapp[bot]", true),
            body: "self release".to_string(),
            html_url: String::new(),
            published_at: at("2026-06-13T01:20:00Z"),
        });
        assert!(to_message(&release, &filter(false, true), false).is_none());
    }

    #[test]
    fn foreign_bot_respects_listen_to_bots() {
        let event = comment_event("dependabot[bot]", true, "@myapp review this", 7);
        assert!(to_message(&event, &filter(true, false), true).is_none());
        assert!(to_message(&event, &filter(true, true), true).is_some());
    }

    #[test]
    fn empty_body_dropped_on_conversational_path() {
        let event = comment_event("test_user", false, "@myapp", 7);
        assert!(to_message(&event, &filter(true, false), true).is_none());
        let event = comment_event("test_user", false, "", 7);
        assert!(to_message(&event, &filter(false, false), true).is_none());
    }

    #[test]
    fn empty_opening_post_synthesizes_content_on_ungated_path() {
        let event = GitEvent::PullRequestOpened(PullPost {
            repo: repo(),
            number: 12,
            author: actor("test_user", false),
            title: "Flaky test".to_string(),
            body: String::new(),
            created_at: at("2026-06-13T01:00:00Z"),
        });
        // Gated (message) path: nothing to say, dropped (v1 behavior).
        assert!(to_message(&event, &filter(true, false), true).is_none());
        // Ungated (sop-degraded) path: the event still matters.
        let msg = to_message(&event, &filter(true, false), false).unwrap();
        assert_eq!(msg.content, "Pull request #12 opened: Flaky test");
    }

    // ── Mention helpers (unchanged v1 behavior) ─────────────────────

    #[test]
    fn mention_requires_word_boundary() {
        assert!(contains_mention("hey @myapp do it", "myapp"));
        assert!(contains_mention("@MyApp case insensitive", "myapp"));
        assert!(!contains_mention("ping @myapp-helper instead", "myapp"));
        assert!(!contains_mention("email me@myapp nothing", "myap"));
    }

    #[test]
    fn strip_mention_keeps_non_boundary_matches() {
        assert_eq!(strip_mention("@myapp do it", "myapp"), "do it");
        assert_eq!(
            strip_mention("cc @myapp-helper stays", "myapp"),
            "cc @myapp-helper stays"
        );
    }

    #[test]
    fn mention_handling_is_safe_on_non_ascii_bodies() {
        // 'İ' (U+0130) lowercases to a longer byte sequence under full
        // Unicode folding; ASCII folding keeps offsets aligned so the
        // mention is found and stripped without slicing mid-character.
        let body = "İstanbul rollout: @myapp ölçüm çalıştır 😀";
        assert!(contains_mention(body, "myapp"));
        assert_eq!(
            strip_mention(body, "myapp"),
            "İstanbul rollout: ölçüm çalıştır 😀"
        );
    }
}
