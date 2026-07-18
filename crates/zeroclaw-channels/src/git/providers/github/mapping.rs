//! GitHub payload → [`GitEvent`] normalization, plus the GitHub reaction
//! map.

use super::payloads::{
    GhComment, GhIssue, GhPull, GhRelease, GhRepoEvent, GhReviewComment, GhUser, GhWorkflowRun,
};
use crate::git::events::{
    CommentPost, EventActor, GitEvent, IssuePost, PullPost, PullTransition, ReleasePost, RunOutcome,
};
use crate::git::types::RepoRef;

/// Normalize a GitHub user payload onto the generic actor model.
fn actor(user: &GhUser) -> EventActor {
    EventActor {
        login: user.login.clone(),
        is_bot: user.is_bot(),
    }
}

/// A new issue/PR comment. `None` when the parent issue number can't be
/// derived (malformed `issue_url`).
pub fn from_comment(comment: &GhComment, repo: &RepoRef) -> Option<GitEvent> {
    Some(GitEvent::IssueCommentCreated(CommentPost {
        repo: repo.clone(),
        number: comment.issue_number()?,
        comment_id: comment.id.to_string(),
        author: actor(&comment.user),
        body: comment.body.clone().unwrap_or_default(),
        created_at: comment.created_at,
    }))
}

/// The opening post of an issue or PR (issues namespace covers both; the
/// embedded stub distinguishes them).
pub fn from_issue_opened(issue: &GhIssue, repo: &RepoRef) -> GitEvent {
    if issue.is_pull_request() {
        GitEvent::PullRequestOpened(PullPost {
            repo: repo.clone(),
            number: issue.number,
            author: actor(&issue.user),
            title: issue.title.clone(),
            body: issue.body.clone().unwrap_or_default(),
            created_at: issue.created_at,
        })
    } else {
        GitEvent::IssueOpened(IssuePost {
            repo: repo.clone(),
            number: issue.number,
            issue_id: issue.id.to_string(),
            author: actor(&issue.user),
            title: issue.title.clone(),
            body: issue.body.clone().unwrap_or_default(),
            created_at: issue.created_at,
        })
    }
}

/// A PR close/merge observed via the issues namespace. `None` for open
/// items and plain issues (issue closes are not a routed event type).
pub fn from_pull_transition(issue: &GhIssue, repo: &RepoRef) -> Option<GitEvent> {
    let stub = issue.pull_request.as_ref()?;
    let at = issue.closed_at?;
    let transition = PullTransition {
        repo: repo.clone(),
        number: issue.number,
        title: issue.title.clone(),
        author: actor(&issue.user),
        html_url: issue.html_url.clone(),
        at,
    };
    Some(if stub.merged_at.is_some() {
        GitEvent::PullRequestMerged(transition)
    } else {
        GitEvent::PullRequestClosed(transition)
    })
}

/// An inline PR review comment. `None` when the parent PR number can't be
/// derived.
pub fn from_review_comment(comment: &GhReviewComment, repo: &RepoRef) -> Option<GitEvent> {
    Some(GitEvent::PullRequestReviewCommentCreated(CommentPost {
        repo: repo.clone(),
        number: comment.pull_number()?,
        comment_id: comment.id.to_string(),
        author: actor(&comment.user),
        body: comment.body.clone().unwrap_or_default(),
        created_at: comment.created_at,
    }))
}

/// A workflow run. `None` while still in flight and for conclusions that
/// are neither success nor failure (cancelled, skipped, …) — those aren't
/// routed event types.
pub fn from_workflow_run(run: &GhWorkflowRun, repo: &RepoRef) -> Option<GitEvent> {
    if run.status != "completed" {
        return None;
    }
    let outcome = RunOutcome {
        repo: repo.clone(),
        run_id: run.id.to_string(),
        attempt: run.run_attempt,
        name: run.name.clone().unwrap_or_else(|| "workflow".to_string()),
        branch: run.head_branch.clone(),
        run_number: run.run_number,
        pr_number: run.pull_requests.first().map(|p| p.number),
        actor: run.actor.as_ref().map(actor),
        html_url: run.html_url.clone(),
        finished_at: run.updated_at,
    };
    match run.conclusion.as_deref() {
        Some("success") => Some(GitEvent::WorkflowRunCompleted(outcome)),
        Some("failure" | "timed_out" | "startup_failure") => {
            Some(GitEvent::WorkflowRunFailed(outcome))
        }
        _ => None,
    }
}

/// A release. `None` for drafts (published events only).
pub fn from_release(release: &GhRelease, repo: &RepoRef) -> Option<GitEvent> {
    if release.draft {
        return None;
    }
    Some(GitEvent::ReleasePublished(ReleasePost {
        repo: repo.clone(),
        release_id: release.id.to_string(),
        tag: release.tag_name.clone(),
        name: release.name.clone(),
        author: actor(&release.author),
        body: release.body.clone().unwrap_or_default(),
        html_url: release.html_url.clone(),
        published_at: release.published_at?,
    }))
}

pub fn from_repo_event(event: &GhRepoEvent, repo: &RepoRef) -> Option<GitEvent> {
    fn embedded<T: serde::de::DeserializeOwned>(
        payload: &serde_json::Value,
        key: &str,
    ) -> Option<T> {
        serde_json::from_value(payload.get(key)?.clone()).ok()
    }
    let action = event
        .payload
        .get("action")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    match (event.kind.as_str(), action) {
        ("IssueCommentEvent", "created") => {
            let comment: GhComment = embedded(&event.payload, "comment")?;
            from_comment(&comment, repo)
        }
        ("IssuesEvent", "opened") => {
            let issue: GhIssue = embedded(&event.payload, "issue")?;
            Some(from_issue_opened(&issue, repo))
        }
        ("PullRequestEvent", "opened") => {
            let pull: GhPull = embedded(&event.payload, "pull_request")?;
            Some(GitEvent::PullRequestOpened(PullPost {
                repo: repo.clone(),
                number: pull.number,
                author: actor(&pull.user),
                title: pull.title.clone(),
                body: pull.body.clone().unwrap_or_default(),
                created_at: pull.created_at,
            }))
        }
        ("PullRequestEvent", "closed") => {
            let pull: GhPull = embedded(&event.payload, "pull_request")?;
            let transition = PullTransition {
                repo: repo.clone(),
                number: pull.number,
                title: pull.title.clone(),
                author: actor(&pull.user),
                html_url: pull.html_url.clone(),
                at: pull.closed_at?,
            };
            Some(if pull.merged_at.is_some() {
                GitEvent::PullRequestMerged(transition)
            } else {
                GitEvent::PullRequestClosed(transition)
            })
        }
        ("PullRequestReviewCommentEvent", "created") => {
            let comment: GhReviewComment = embedded(&event.payload, "comment")?;
            from_review_comment(&comment, repo)
        }
        ("ReleaseEvent", "published") => {
            let release: GhRelease = embedded(&event.payload, "release")?;
            from_release(&release, repo)
        }
        _ => None,
    }
}

/// Map a channel emoji onto GitHub's fixed reaction set. Unmappable
/// emoji are dropped by the caller (reaction support is best-effort).
pub fn map_reaction(emoji: &str) -> Option<&'static str> {
    match emoji {
        "👍" | "+1" | "✅" => Some("+1"),
        "👎" | "-1" => Some("-1"),
        "😀" | "😄" | "😆" => Some("laugh"),
        "😕" | "⚠️" => Some("confused"),
        "❤️" | "💜" => Some("heart"),
        "🎉" => Some("hooray"),
        "🚀" => Some("rocket"),
        "👀" => Some("eyes"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> RepoRef {
        RepoRef::parse("octo/repo").unwrap()
    }

    /// Captured-shape REST fixture: an issue comment.
    fn comment_json(login: &str, kind: &str, body: &str) -> serde_json::Value {
        serde_json::json!({
            "id": 9001,
            "body": body,
            "user": {"login": login, "type": kind},
            "created_at": "2026-06-13T01:02:03Z",
            "issue_url": "https://api.github.com/repos/octo/repo/issues/7",
        })
    }

    fn comment(login: &str, kind: &str, body: &str) -> GhComment {
        serde_json::from_value(comment_json(login, kind, body)).unwrap()
    }

    /// Captured-shape REST fixture: an issue (or PR when `pr` is set).
    fn issue_json(pr: bool, closed: bool, merged: bool) -> serde_json::Value {
        let mut v = serde_json::json!({
            "id": 555,
            "number": 12,
            "title": "Flaky test",
            "body": "@myapp please investigate",
            "user": {"login": "test_user", "type": "User"},
            "created_at": "2026-06-13T01:00:00Z",
            "html_url": "https://github.com/octo/repo/issues/12",
        });
        if pr {
            v["pull_request"] = serde_json::json!({
                "url": "https://api.github.com/repos/octo/repo/pulls/12",
                "merged_at": if merged { serde_json::json!("2026-06-13T02:00:00Z") } else { serde_json::Value::Null },
            });
        }
        if closed {
            v["closed_at"] = serde_json::json!("2026-06-13T02:00:00Z");
        }
        v
    }

    fn issue(pr: bool, closed: bool, merged: bool) -> GhIssue {
        serde_json::from_value(issue_json(pr, closed, merged)).unwrap()
    }

    /// Captured-shape REST fixture: an inline PR review comment.
    fn review_comment_json() -> serde_json::Value {
        serde_json::json!({
            "id": 4242,
            "body": "@myapp is this cast safe?",
            "user": {"login": "test_user", "type": "User"},
            "created_at": "2026-06-13T01:05:00Z",
            "pull_request_url": "https://api.github.com/repos/octo/repo/pulls/31",
        })
    }

    /// Captured-shape REST fixture: a workflow run.
    fn run_json(status: &str, conclusion: Option<&str>) -> serde_json::Value {
        serde_json::json!({
            "id": 77001,
            "name": "CI",
            "status": status,
            "conclusion": conclusion,
            "created_at": "2026-06-13T01:10:00Z",
            "updated_at": "2026-06-13T01:14:00Z",
            "html_url": "https://github.com/octo/repo/actions/runs/77001",
            "head_branch": "feat/x",
            "run_number": 88,
            "run_attempt": 1,
            "actor": {"login": "test_user", "type": "User"},
            "pull_requests": [{"number": 31}],
        })
    }

    /// Captured-shape REST fixture: a release.
    fn release_json(draft: bool, body: &str) -> serde_json::Value {
        serde_json::json!({
            "id": 6100,
            "tag_name": "v0.9.0",
            "name": "v0.9.0 — events",
            "body": body,
            "author": {"login": "test_user", "type": "User"},
            "draft": draft,
            "published_at": if draft { serde_json::Value::Null } else { serde_json::json!("2026-06-13T01:20:00Z") },
            "html_url": "https://github.com/octo/repo/releases/tag/v0.9.0",
        })
    }

    // ── Fixture → typed event, per event type ───────────────────────

    #[test]
    fn issue_comment_normalizes_to_generic_event() {
        let event = from_comment(
            &comment("test_user", "User", "@myapp run the tests"),
            &repo(),
        )
        .unwrap();
        assert_eq!(event.event_type(), "issue_comment.created");
        assert_eq!(event.dedup_id(), "ghc_9001");
        assert!(event.is_conversational());
        assert_eq!(event.author().unwrap().login, "test_user");
    }

    #[test]
    fn opening_posts_split_on_the_pull_request_stub() {
        let plain = from_issue_opened(&issue(false, false, false), &repo());
        assert_eq!(plain.event_type(), "issues.opened");
        assert_eq!(plain.dedup_id(), "ghi_555");

        let pr = from_issue_opened(&issue(true, false, false), &repo());
        assert_eq!(pr.event_type(), "pull_request.opened");
        // PRs have no transport-stable object id; identity is repo#number.
        assert_eq!(pr.dedup_id(), "ghpr_octo/repo#12");
    }

    #[test]
    fn pull_transitions_split_on_merged_at() {
        let merged = from_pull_transition(&issue(true, true, true), &repo()).unwrap();
        assert_eq!(merged.event_type(), "pull_request.merged");
        let closed = from_pull_transition(&issue(true, true, false), &repo()).unwrap();
        assert_eq!(closed.event_type(), "pull_request.closed");
        assert_eq!(closed.dedup_id(), merged.dedup_id());

        // Open PRs and plain issues produce no transition.
        assert!(from_pull_transition(&issue(true, false, false), &repo()).is_none());
        assert!(from_pull_transition(&issue(false, true, false), &repo()).is_none());
    }

    #[test]
    fn review_comment_maps_to_the_pull_request() {
        let rc: GhReviewComment = serde_json::from_value(review_comment_json()).unwrap();
        let event = from_review_comment(&rc, &repo()).unwrap();
        assert_eq!(event.event_type(), "pull_request_review_comment.created");
        assert_eq!(event.dedup_id(), "ghrc_4242");
        assert!(event.is_conversational());
    }

    #[test]
    fn review_comment_with_malformed_pull_url_is_dropped() {
        let mut json = review_comment_json();
        json["pull_request_url"] = serde_json::json!("not-a-url");
        let rc: GhReviewComment = serde_json::from_value(json).unwrap();
        assert!(from_review_comment(&rc, &repo()).is_none());
    }

    #[test]
    fn workflow_runs_map_only_terminal_conclusions() {
        let parse = |status: &str, conclusion: Option<&str>| -> Option<GitEvent> {
            let run: GhWorkflowRun = serde_json::from_value(run_json(status, conclusion)).unwrap();
            from_workflow_run(&run, &repo())
        };
        let ok = parse("completed", Some("success")).unwrap();
        assert_eq!(ok.event_type(), "workflow_run.completed");
        let failed = parse("completed", Some("failure")).unwrap();
        assert_eq!(failed.event_type(), "workflow_run.failed");
        assert_eq!(failed.dedup_id(), "ghwr_77001_1");
        assert!(!failed.is_conversational());
        assert!(
            parse("completed", Some("timed_out")).unwrap().event_type() == "workflow_run.failed"
        );
        // In-flight and non-verdict conclusions are not events.
        assert!(parse("in_progress", None).is_none());
        assert!(parse("completed", Some("cancelled")).is_none());
        assert!(parse("completed", None).is_none());
    }

    #[test]
    fn releases_map_published_only() {
        let published: GhRelease =
            serde_json::from_value(release_json(false, "Changelog body")).unwrap();
        let event = from_release(&published, &repo()).unwrap();
        assert_eq!(event.event_type(), "release.published");
        assert_eq!(event.dedup_id(), "ghrel_6100");

        let draft: GhRelease = serde_json::from_value(release_json(true, "wip")).unwrap();
        assert!(from_release(&draft, &repo()).is_none());
    }

    #[test]
    fn malformed_comment_issue_url_is_dropped() {
        let mut c = comment("test_user", "User", "@myapp hello");
        c.issue_url = "garbage".to_string();
        assert!(from_comment(&c, &repo()).is_none());
    }

    // ── Tier C feed entries: same identity as the targeted endpoints ──

    fn feed_event(kind: &str, payload: serde_json::Value) -> GhRepoEvent {
        serde_json::from_value(serde_json::json!({
            "id": "36734073180",
            "type": kind,
            "created_at": "2026-06-13T01:06:00Z",
            "payload": payload,
        }))
        .unwrap()
    }

    #[test]
    fn feed_comment_shares_identity_with_targeted_poll() {
        let json = comment_json("test_user", "User", "@myapp ping");
        let targeted =
            from_comment(&serde_json::from_value(json.clone()).unwrap(), &repo()).unwrap();
        let feed = from_repo_event(
            &feed_event(
                "IssueCommentEvent",
                serde_json::json!({"action": "created", "issue": issue_json(false, false, false), "comment": json}),
            ),
            &repo(),
        )
        .unwrap();
        assert_eq!(feed.dedup_id(), targeted.dedup_id());
        assert_eq!(feed.event_type(), targeted.event_type());
    }

    #[test]
    fn feed_pull_request_events_share_identity_with_issue_view() {
        // The feed carries the pull-view object (different id space from
        // the issue view) — identity must still line up.
        let pull = serde_json::json!({
            "id": 999777, // pull-view id, deliberately != issue-view 555
            "number": 12,
            "title": "Flaky test",
            "body": "@myapp please investigate",
            "user": {"login": "test_user", "type": "User"},
            "created_at": "2026-06-13T01:00:00Z",
            "closed_at": null,
            "merged_at": null,
            "html_url": "https://github.com/octo/repo/pull/12",
        });
        let opened = from_repo_event(
            &feed_event(
                "PullRequestEvent",
                serde_json::json!({"action": "opened", "pull_request": pull}),
            ),
            &repo(),
        )
        .unwrap();
        let issue_view = from_issue_opened(&issue(true, false, false), &repo());
        assert_eq!(opened.dedup_id(), issue_view.dedup_id());

        let mut merged_pull = pull.clone();
        merged_pull["closed_at"] = serde_json::json!("2026-06-13T02:00:00Z");
        merged_pull["merged_at"] = serde_json::json!("2026-06-13T02:00:00Z");
        let merged = from_repo_event(
            &feed_event(
                "PullRequestEvent",
                serde_json::json!({"action": "closed", "pull_request": merged_pull}),
            ),
            &repo(),
        )
        .unwrap();
        assert_eq!(merged.event_type(), "pull_request.merged");
        let issue_view = from_pull_transition(&issue(true, true, true), &repo()).unwrap();
        assert_eq!(merged.dedup_id(), issue_view.dedup_id());
    }

    #[test]
    fn feed_release_event_maps() {
        let feed = from_repo_event(
            &feed_event(
                "ReleaseEvent",
                serde_json::json!({"action": "published", "release": release_json(false, "notes")}),
            ),
            &repo(),
        )
        .unwrap();
        assert_eq!(feed.event_type(), "release.published");
        assert_eq!(feed.dedup_id(), "ghrel_6100");
    }

    #[test]
    fn feed_ignores_unknown_kinds_actions_and_malformed_payloads() {
        // Unsupported feed type.
        assert!(
            from_repo_event(
                &feed_event("WatchEvent", serde_json::json!({"action": "started"})),
                &repo()
            )
            .is_none()
        );
        // Supported type, unsupported action (edits are ignored by design).
        assert!(from_repo_event(
            &feed_event(
                "IssueCommentEvent",
                serde_json::json!({"action": "edited", "comment": comment_json("test_user", "User", "hi")})
            ),
            &repo()
        )
        .is_none());
        // Trimmed payload missing the embedded object.
        assert!(
            from_repo_event(
                &feed_event(
                    "IssueCommentEvent",
                    serde_json::json!({"action": "created"})
                ),
                &repo()
            )
            .is_none()
        );
        // Embedded object that doesn't parse.
        assert!(
            from_repo_event(
                &feed_event(
                    "ReleaseEvent",
                    serde_json::json!({"action": "published", "release": {"id": "not-a-number"}})
                ),
                &repo()
            )
            .is_none()
        );
    }

    #[test]
    fn reaction_map_covers_ack_flow() {
        assert_eq!(map_reaction("👀"), Some("eyes"));
        assert_eq!(map_reaction("✅"), Some("+1"));
        assert_eq!(map_reaction("⚠️"), Some("confused"));
        assert_eq!(map_reaction("🦖"), None);
    }
}
