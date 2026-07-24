//! `GiteaProvider` — Gitea/Forgejo implementation of [`GitProvider`].

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use super::api::GiteaApi;
use super::mapping;
use crate::git::poll::PollStream;
use crate::git::traits::{FetchPage, GitProvider, ReactionTarget, SelfIdentity};
use crate::git::types::{GitChannelError, IssueRef, RepoRef};

pub struct GiteaProvider {
    api: GiteaApi,
    access_token: String,
    identity: parking_lot::Mutex<Option<SelfIdentity>>,
}

impl GiteaProvider {
    pub fn new(api_base_url: String, access_token: String, proxy_url: Option<String>) -> Self {
        Self {
            api: GiteaApi::new(api_base_url, proxy_url),
            access_token,
            identity: parking_lot::Mutex::new(None),
        }
    }

    fn token(&self) -> Result<&str, GitChannelError> {
        if self.access_token.trim().is_empty() {
            return Err(GitChannelError::Config(
                "gitea/forgejo provider requires channels.git.<alias>.access_token".into(),
            ));
        }
        Ok(self.access_token.as_str())
    }

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
            let mut newest = issue.created_at;
            if let Some(transition) = mapping::from_pull_transition(&issue, repo) {
                newest = newest.max(transition.created_at());
                events.push(transition);
            }
            advance_to = Some(advance_to.map_or(newest, |m| m.max(newest)));
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

    async fn fetch_releases(
        &self,
        token: &str,
        repo: &RepoRef,
        since: DateTime<Utc>,
    ) -> Result<FetchPage, GitChannelError> {
        let mut events = Vec::new();
        let mut advance_to: Option<DateTime<Utc>> = None;
        for release in self.api.list_releases(token, repo).await? {
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
}

#[async_trait]
impl GitProvider for GiteaProvider {
    fn name(&self) -> &'static str {
        "gitea"
    }

    async fn self_identity(&self) -> Result<SelfIdentity, GitChannelError> {
        if let Some(id) = self.identity.lock().as_ref() {
            return Ok(SelfIdentity {
                mention_handle: id.mention_handle.clone(),
                bot_login: id.bot_login.clone(),
            });
        }
        let user = self.api.current_user(self.token()?).await?;
        let login = user.login();
        let id = SelfIdentity {
            mention_handle: login.clone(),
            bot_login: login,
        };
        *self.identity.lock() = Some(SelfIdentity {
            mention_handle: id.mention_handle.clone(),
            bot_login: id.bot_login.clone(),
        });
        Ok(id)
    }

    async fn discover_repos(&self) -> Result<Vec<RepoRef>, GitChannelError> {
        self.api.list_user_repos(self.token()?).await
    }

    async fn fetch(
        &self,
        repo: &RepoRef,
        stream: PollStream,
        since: DateTime<Utc>,
        _etag: Option<&str>,
    ) -> Result<FetchPage, GitChannelError> {
        let token = self.token()?;
        match stream {
            PollStream::Issues => self.fetch_issues(token, repo, since).await,
            PollStream::Comments => self.fetch_comments(token, repo, since).await,
            PollStream::Releases => self.fetch_releases(token, repo, since).await,
            PollStream::ReviewComments | PollStream::WorkflowRuns | PollStream::Feed => {
                Ok(FetchPage {
                    events: Vec::new(),
                    advance_to: None,
                    etag: None,
                    not_modified: false,
                })
            }
        }
    }

    async fn post_comment(&self, target: &IssueRef, body: &str) -> Result<String, GitChannelError> {
        let id = self.api.create_comment(self.token()?, target, body).await?;
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
        self.api.update_comment(self.token()?, repo, id, body).await
    }

    async fn delete_comment(
        &self,
        repo: &RepoRef,
        comment_id: &str,
    ) -> Result<(), GitChannelError> {
        let id: u64 = comment_id
            .parse()
            .map_err(|_| GitChannelError::BadRecipient(comment_id.to_string()))?;
        self.api.delete_comment(self.token()?, repo, id).await
    }

    async fn add_reaction(
        &self,
        target: &ReactionTarget,
        emoji: &str,
    ) -> Result<(), GitChannelError> {
        let Some(content) = mapping::map_reaction(emoji) else {
            return Ok(());
        };
        match target {
            ReactionTarget::Comment { repo, comment_id } => {
                let Ok(id) = comment_id.parse::<u64>() else {
                    return Ok(());
                };
                self.api
                    .add_comment_reaction(self.token()?, repo, id, content)
                    .await
            }
            ReactionTarget::Issue(issue) => {
                self.api
                    .add_issue_reaction(self.token()?, issue, content)
                    .await
            }
        }
    }

    async fn forge_request(
        &self,
        req: crate::git::types::ForgeRequest,
    ) -> Result<crate::git::types::ForgeResponse, GitChannelError> {
        let req = translate_to_gitea(req);
        let method = crate::git::providers::forge_method_to_reqwest(req.method);
        let (status, body) = self
            .api
            .forge_call(self.token()?, method, &req.path, req.body.as_ref())
            .await?;
        Ok(crate::git::types::ForgeResponse { status, body })
    }
}

/// Rewrite a GitHub-canonical [`ForgeRequest`] into Gitea/Forgejo's dialect.
///
/// The `git_forge` tool speaks one canonical vocabulary (GitHub's); the forge
/// that diverges translates on the way through, so forge-specific knowledge
/// lives behind the provider, not in the tool. Every rewrite below is grounded
/// in Gitea's OpenAPI/source: the merge endpoint is `POST` with a `Do` verb and
/// `MergeTitleField`/`MergeMessageField` (GitHub uses `PUT` +
/// `merge_method`/`commit_title`/`commit_message`); `EditIssueOption` has no
/// `state_reason`; and `ReviewStateType` spells approval `APPROVED`, not
/// GitHub's `APPROVE`. Shapes Gitea shares with GitHub (pull close via
/// `{state:closed}`, labels, milestone id, comments) pass through untouched.
fn translate_to_gitea(req: crate::git::types::ForgeRequest) -> crate::git::types::ForgeRequest {
    use crate::git::types::ForgeMethod;
    use serde_json::Value;

    let crate::git::types::ForgeRequest { method, path, body } = req;

    let is_merge = method == ForgeMethod::Put && path.ends_with("/merge");
    if is_merge {
        let mut out = serde_json::Map::new();
        if let Some(Value::Object(src)) = &body {
            if let Some(Value::String(m)) = src.get("merge_method") {
                let door = if m == "rebase" {
                    "rebase-merge"
                } else {
                    m.as_str()
                };
                out.insert("Do".into(), Value::String(door.to_string()));
            }
            if let Some(t) = src.get("commit_title") {
                out.insert("MergeTitleField".into(), t.clone());
            }
            if let Some(msg) = src.get("commit_message") {
                out.insert("MergeMessageField".into(), msg.clone());
            }
        }
        if !out.contains_key("Do") {
            out.insert("Do".into(), Value::String("merge".into()));
        }
        return crate::git::types::ForgeRequest {
            method: ForgeMethod::Post,
            path,
            body: Some(Value::Object(out)),
        };
    }

    let mut body = body;
    if let Some(Value::Object(map)) = &mut body {
        map.remove("state_reason");
        if let Some(Value::String(event)) = map.get_mut("event")
            && event == "APPROVE"
        {
            *event = "APPROVED".to_string();
        }
    }

    crate::git::types::ForgeRequest { method, path, body }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn translate_merge_maps_method_verb_and_body_keys() {
        use crate::git::types::{ForgeMethod, ForgeRequest};
        let req = ForgeRequest {
            method: ForgeMethod::Put,
            path: "repos/o/r/pulls/5/merge".into(),
            body: Some(serde_json::json!({
                "merge_method": "squash",
                "commit_title": "feat: x (#5)",
                "commit_message": "- abc"
            })),
        };
        let out = translate_to_gitea(req);
        assert_eq!(out.method, ForgeMethod::Post);
        let body = out.body.unwrap();
        assert_eq!(body["Do"], "squash");
        assert_eq!(body["MergeTitleField"], "feat: x (#5)");
        assert_eq!(body["MergeMessageField"], "- abc");
        assert!(body.get("merge_method").is_none());
    }

    #[test]
    fn translate_merge_rebase_becomes_rebase_merge() {
        use crate::git::types::{ForgeMethod, ForgeRequest};
        let req = ForgeRequest {
            method: ForgeMethod::Put,
            path: "repos/o/r/pulls/5/merge".into(),
            body: Some(serde_json::json!({ "merge_method": "rebase" })),
        };
        let out = translate_to_gitea(req);
        assert_eq!(out.body.unwrap()["Do"], "rebase-merge");
    }

    #[test]
    fn translate_strips_issue_state_reason() {
        use crate::git::types::{ForgeMethod, ForgeRequest};
        let req = ForgeRequest {
            method: ForgeMethod::Patch,
            path: "repos/o/r/issues/12".into(),
            body: Some(serde_json::json!({ "state": "closed", "state_reason": "not_planned" })),
        };
        let out = translate_to_gitea(req);
        let body = out.body.unwrap();
        assert_eq!(body["state"], "closed");
        assert!(body.get("state_reason").is_none());
    }

    #[test]
    fn translate_review_approve_becomes_approved() {
        use crate::git::types::{ForgeMethod, ForgeRequest};
        let req = ForgeRequest {
            method: ForgeMethod::Post,
            path: "repos/o/r/pulls/5/reviews".into(),
            body: Some(serde_json::json!({ "event": "APPROVE", "body": "lgtm" })),
        };
        let out = translate_to_gitea(req);
        assert_eq!(out.body.unwrap()["event"], "APPROVED");
    }

    #[test]
    fn translate_pull_close_passes_through_untouched() {
        use crate::git::types::{ForgeMethod, ForgeRequest};
        let req = ForgeRequest {
            method: ForgeMethod::Patch,
            path: "repos/o/r/pulls/5".into(),
            body: Some(serde_json::json!({ "state": "closed" })),
        };
        let out = translate_to_gitea(req);
        assert_eq!(out.method, ForgeMethod::Patch);
        assert_eq!(out.body.unwrap()["state"], "closed");
    }

    // Regression: real Gitea `/user` returns BOTH `login` and `username`
    // (same value); parsing must not fail with "duplicate field". Forgejo/older
    // builds may send only one.
    #[test]
    fn gitea_user_accepts_both_login_and_username() {
        use super::super::payloads::GiteaUser;
        let both: GiteaUser =
            serde_json::from_str(r#"{"id":6,"login":"botbot","username":"botbot"}"#).unwrap();
        assert_eq!(both.login(), "botbot");
        let only_username: GiteaUser = serde_json::from_str(r#"{"username":"solo"}"#).unwrap();
        assert_eq!(only_username.login(), "solo");
    }

    fn issue_json(
        id: u64,
        created: &str,
        closed: Option<&str>,
        merged_at: Option<&str>,
    ) -> serde_json::Value {
        let mut v = serde_json::json!({
            "id": id,
            "number": id,
            "title": format!("issue {id}"),
            "user": {"login": "alice", "type": "User"},
            "created_at": created,
            "html_url": format!("https://git.example.org/octo/repo/issues/{id}"),
        });
        if let Some(c) = closed {
            v["closed_at"] = serde_json::json!(c);
        }
        if let Some(m) = merged_at {
            v["pull_request"] = serde_json::json!({"merged": true, "merged_at": m});
        }
        v
    }

    // Regression for the first-page-only bug: `list_issues_since` hard-coded
    // `page=1`, so any repo with more than one page of matching issues silently
    // dropped everything past the first page. A full page (100) must trigger a
    // fetch of page 2.
    #[tokio::test]
    async fn gitea_list_issues_paginates_beyond_first_page() {
        let server = MockServer::start().await;
        let now = chrono::Utc::now();
        let created = (now - chrono::Duration::minutes(5)).to_rfc3339();
        let page1: Vec<_> = (1..=100)
            .map(|i| issue_json(i, &created, None, None))
            .collect();
        let page2: Vec<_> = (101..=103)
            .map(|i| issue_json(i, &created, None, None))
            .collect();
        Mock::given(method("GET"))
            .and(path("/repos/octo/repo/issues"))
            .and(query_param("page", "1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(page1))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/repos/octo/repo/issues"))
            .and(query_param("page", "2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(page2))
            .mount(&server)
            .await;

        let provider = GiteaProvider::new(server.uri(), "t".into(), None);
        let repo = RepoRef::parse("octo/repo").unwrap();
        let page = provider
            .fetch_issues("t", &repo, now - chrono::Duration::hours(1))
            .await
            .unwrap();
        assert_eq!(
            page.events.len(),
            103,
            "must collect issues across both pages, not just the first"
        );
    }

    // Live smoke against a real Gitea/Forgejo instance. Ignored by default;
    // run with `GITEA_TEST_BASE` + `GITEA_TEST_TOKEN` set and `-- --ignored
    // --nocapture`. Exercises identity resolution + an issue poll end-to-end.
    #[tokio::test]
    #[ignore = "requires live Gitea creds via GITEA_TEST_BASE + GITEA_TEST_TOKEN"]
    async fn gitea_live_identity_and_poll() {
        let (Ok(base), Ok(token)) = (
            std::env::var("GITEA_TEST_BASE"),
            std::env::var("GITEA_TEST_TOKEN"),
        ) else {
            eprintln!("SKIP: set GITEA_TEST_BASE + GITEA_TEST_TOKEN");
            return;
        };
        let repo_str = std::env::var("GITEA_TEST_REPO").unwrap_or_else(|_| "Nillth/Hello".into());
        let provider = GiteaProvider::new(base, token, None);
        match provider.self_identity().await {
            Ok(id) => eprintln!(
                "IDENTITY OK: bot_login={} mention={}",
                id.bot_login, id.mention_handle
            ),
            Err(e) => eprintln!("IDENTITY ERR: {e:?}"),
        }
        let repo = RepoRef::parse(&repo_str).unwrap();
        let since = chrono::Utc::now() - chrono::Duration::days(3650);
        match provider.fetch_issues("", &repo, since).await {
            Ok(page) => eprintln!(
                "POLL OK: {} events, advance_to={:?}",
                page.events.len(),
                page.advance_to
            ),
            Err(e) => eprintln!("POLL ERR: {e:?}"),
        }
    }

    // Regression for the cursor bug: a PR created before the cursor but
    // closed/merged after it emits a transition timed at `closed_at`; the cursor
    // must advance past that transition, not stall at `created_at` (which would
    // re-fetch the same transition every tick).
    #[tokio::test]
    async fn gitea_issue_cursor_advances_past_close_after_cursor() {
        let server = MockServer::start().await;
        let now = chrono::Utc::now();
        let created = now - chrono::Duration::hours(2); // before the cursor
        let cursor = now - chrono::Duration::hours(1);
        let closed = now; // after the cursor
        let issue = issue_json(
            1,
            &created.to_rfc3339(),
            Some(&closed.to_rfc3339()),
            Some(&closed.to_rfc3339()),
        );
        Mock::given(method("GET"))
            .and(path("/repos/octo/repo/issues"))
            .respond_with(ResponseTemplate::new(200).set_body_json(vec![issue]))
            .mount(&server)
            .await;

        let provider = GiteaProvider::new(server.uri(), "t".into(), None);
        let repo = RepoRef::parse("octo/repo").unwrap();
        let page = provider.fetch_issues("t", &repo, cursor).await.unwrap();
        assert_eq!(
            page.events.len(),
            2,
            "opened + merged transition both emitted"
        );
        assert_eq!(
            page.advance_to,
            Some(closed),
            "cursor must advance to the merge time, not created_at"
        );
    }
}
