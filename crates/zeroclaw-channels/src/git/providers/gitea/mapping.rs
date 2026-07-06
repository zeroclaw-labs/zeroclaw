//! Gitea-compatible payload normalization.

use super::payloads::{GiteaComment, GiteaIssue, GiteaRelease, GiteaUser};
use crate::git::events::{
    CommentPost, EventActor, GitEvent, IssuePost, PullPost, PullTransition, ReleasePost,
};
use crate::git::types::RepoRef;

fn actor(user: &GiteaUser) -> EventActor {
    EventActor {
        login: user.login(),
        is_bot: user.is_bot(),
    }
}

pub fn from_comment(comment: &GiteaComment, repo: &RepoRef) -> Option<GitEvent> {
    Some(GitEvent::IssueCommentCreated(CommentPost {
        repo: repo.clone(),
        number: comment.issue_number()?,
        comment_id: comment.id.to_string(),
        author: actor(&comment.user),
        body: comment.body.clone(),
        created_at: comment.created_at,
    }))
}

pub fn from_issue_opened(issue: &GiteaIssue, repo: &RepoRef) -> GitEvent {
    if issue.is_pull_request() {
        GitEvent::PullRequestOpened(PullPost {
            repo: repo.clone(),
            number: issue.index,
            author: actor(&issue.user),
            title: issue.title.clone(),
            body: issue.body.clone(),
            created_at: issue.created_at,
        })
    } else {
        GitEvent::IssueOpened(IssuePost {
            repo: repo.clone(),
            number: issue.index,
            issue_id: issue.id.to_string(),
            author: actor(&issue.user),
            title: issue.title.clone(),
            body: issue.body.clone(),
            created_at: issue.created_at,
        })
    }
}

pub fn from_pull_transition(issue: &GiteaIssue, repo: &RepoRef) -> Option<GitEvent> {
    let stub = issue.pull_request.as_ref()?;
    let at = issue.closed_at?;
    let transition = PullTransition {
        repo: repo.clone(),
        number: issue.index,
        title: issue.title.clone(),
        author: actor(&issue.user),
        html_url: issue.html_url.clone(),
        at,
    };
    Some(if stub.merged || stub.merged_at.is_some() {
        GitEvent::PullRequestMerged(transition)
    } else {
        GitEvent::PullRequestClosed(transition)
    })
}

pub fn from_release(release: &GiteaRelease, repo: &RepoRef) -> Option<GitEvent> {
    if release.draft {
        return None;
    }
    Some(GitEvent::ReleasePublished(ReleasePost {
        repo: repo.clone(),
        release_id: release.id.to_string(),
        tag: release.tag_name.clone(),
        name: release.name.clone(),
        author: actor(&release.author),
        body: release.body.clone(),
        html_url: release.html_url.clone(),
        published_at: release.published_at?,
    }))
}

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
        RepoRef::parse("forge/project").unwrap()
    }

    #[test]
    fn issue_payload_splits_issues_and_pull_requests() {
        let issue: GiteaIssue = serde_json::from_value(serde_json::json!({
            "id": 10,
            "index": 7,
            "title": "Bug",
            "body": "Please look",
            "user": {"login": "test_user"},
            "created_at": "2026-06-13T01:00:00Z",
            "html_url": "https://forgejo.example/forge/project/issues/7"
        }))
        .unwrap();
        assert_eq!(
            from_issue_opened(&issue, &repo()).event_type(),
            "issues.opened"
        );

        let pr: GiteaIssue = serde_json::from_value(serde_json::json!({
            "id": 11,
            "index": 8,
            "title": "Patch",
            "body": "Please review",
            "user": {"login": "test_user"},
            "created_at": "2026-06-13T01:00:00Z",
            "pull_request": {"merged": false},
            "html_url": "https://forgejo.example/forge/project/pulls/8"
        }))
        .unwrap();
        assert_eq!(
            from_issue_opened(&pr, &repo()).event_type(),
            "pull_request.opened"
        );
    }

    #[test]
    fn comment_extracts_issue_number_from_issue_url() {
        let comment: GiteaComment = serde_json::from_value(serde_json::json!({
            "id": 99,
            "body": "@bot hello",
            "user": {"login": "alice"},
            "created_at": "2026-06-13T01:05:00Z",
            "issue_url": "https://forgejo.example/api/v1/repos/forge/project/issues/7"
        }))
        .unwrap();
        let event = from_comment(&comment, &repo()).unwrap();
        assert_eq!(event.event_type(), "issue_comment.created");
        assert_eq!(event.dedup_id(), "ghc_99");
    }
}
