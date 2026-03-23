use super::traits::{Tool, ToolResult};
use crate::config::schema::GitHubConfig;
use crate::security::{policy::ToolOperation, SecurityPolicy};
use async_trait::async_trait;
use reqwest::Client;
use serde_json::{json, Value};
use std::sync::Arc;

const MAX_ERROR_BODY_CHARS: usize = 500;

pub struct GitHubTool {
    token: String,
    api_url: String,
    allowed_actions: Vec<String>,
    http: Client,
    security: Arc<SecurityPolicy>,
    timeout_secs: u64,
    default_owner: Option<String>,
}

impl GitHubTool {
    pub fn new(config: GitHubConfig, security: Arc<SecurityPolicy>) -> Self {
        let token = if config.token.is_empty() {
            std::env::var("GITHUB_TOKEN").unwrap_or_default()
        } else {
            config.token
        };

        Self {
            token,
            api_url: config.api_url.trim_end_matches('/').to_string(),
            allowed_actions: config.allowed_actions,
            http: Client::new(),
            security,
            timeout_secs: config.timeout_secs,
            default_owner: config.default_owner,
        }
    }

    fn is_action_allowed(&self, action: &str) -> bool {
        self.allowed_actions.iter().any(|a| a == action)
    }

    fn resolve_owner(&self, owner: Option<&str>) -> Option<String> {
        owner
            .map(String::from)
            .or_else(|| self.default_owner.clone())
    }

    async fn get_repo(&self, owner: &str, repo: &str) -> anyhow::Result<ToolResult> {
        let url = format!("{}/repos/{}/{}", self.api_url, owner, repo);
        let resp = self
            .http
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github.v3+json")
            .timeout(std::time::Duration::from_secs(self.timeout_secs))
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("GitHub get_repo request failed: {e}"))?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!(
                "GitHub get_repo failed ({status}): {}",
                crate::util::truncate_with_ellipsis(&text, MAX_ERROR_BODY_CHARS)
            );
        }

        let raw: Value = resp
            .json()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to parse GitHub get_repo response: {e}"))?;

        let shaped = json!({
            "name": raw["name"],
            "full_name": raw["full_name"],
            "description": raw["description"],
            "private": raw["private"],
            "html_url": raw["html_url"],
            "default_branch": raw["default_branch"],
            "stargazers_count": raw["stargazers_count"],
            "forks_count": raw["forks_count"],
            "open_issues_count": raw["open_issues_count"],
            "language": raw["language"],
            "pushed_at": raw["pushed_at"],
            "topics": raw["topics"],
        });

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&shaped)
                .unwrap_or_else(|_| serde_json::to_string(&shaped).unwrap_or_default()),
            error: None,
        })
    }

    async fn list_repos(
        &self,
        owner: Option<&str>,
        type_: Option<&str>,
    ) -> anyhow::Result<ToolResult> {
        let url = if let Some(o) = self.resolve_owner(owner) {
            format!("{}/users/{}/repos", self.api_url, o)
        } else {
            format!("{}/user/repos", self.api_url)
        };

        let query: Vec<(&str, &str)> = if let Some(t) = type_ {
            vec![("type", t), ("sort", "updated"), ("per_page", "30")]
        } else {
            vec![("sort", "updated"), ("per_page", "30")]
        };

        let resp = self
            .http
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github.v3+json")
            .query(&query)
            .timeout(std::time::Duration::from_secs(self.timeout_secs))
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("GitHub list_repos request failed: {e}"))?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!(
                "GitHub list_repos failed ({status}): {}",
                crate::util::truncate_with_ellipsis(&text, MAX_ERROR_BODY_CHARS)
            );
        }

        let repos: Vec<Value> = resp
            .json()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to parse GitHub list_repos response: {e}"))?;

        let shaped: Vec<Value> = repos
            .iter()
            .map(|r| {
                json!({
                    "name": r["name"],
                    "full_name": r["full_name"],
                    "description": r["description"],
                    "private": r["private"],
                    "html_url": r["html_url"],
                    "default_branch": r["default_branch"],
                    "stargazers_count": r["stargazers_count"],
                    "open_issues_count": r["open_issues_count"],
                    "language": r["language"],
                    "updated_at": r["updated_at"],
                })
            })
            .collect();

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&shaped)
                .unwrap_or_else(|_| serde_json::to_string(&shaped).unwrap_or_default()),
            error: None,
        })
    }

    async fn list_issues(
        &self,
        owner: &str,
        repo: &str,
        state: Option<&str>,
        labels: Option<&str>,
        max_results: Option<u32>,
    ) -> anyhow::Result<ToolResult> {
        let url = format!("{}/repos/{}/{}/issues", self.api_url, owner, repo);
        let per_page = max_results.unwrap_or(25).min(100).to_string();
        let mut query: Vec<(&str, &str)> = vec![
            ("state", state.unwrap_or("open")),
            ("sort", "updated"),
            ("direction", "desc"),
            ("per_page", &per_page),
        ];

        if let Some(l) = labels {
            query.push(("labels", l));
        }

        let resp = self
            .http
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github.v3+json")
            .query(&query)
            .timeout(std::time::Duration::from_secs(self.timeout_secs))
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("GitHub list_issues request failed: {e}"))?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!(
                "GitHub list_issues failed ({status}): {}",
                crate::util::truncate_with_ellipsis(&text, MAX_ERROR_BODY_CHARS)
            );
        }

        let issues: Vec<Value> = resp
            .json()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to parse GitHub list_issues response: {e}"))?;

        let shaped: Vec<Value> = issues
            .iter()
            .filter(|i| i["pull_request"].is_null())
            .map(|i| {
                json!({
                    "number": i["number"],
                    "title": i["title"],
                    "state": i["state"],
                    "html_url": i["html_url"],
                    "user": i["user"]["login"],
                    "labels": i["labels"].as_array().map(|arr| arr.iter().map(|l| l["name"].clone()).collect::<Vec<_>>()),
                    "assignees": i["assignees"].as_array().map(|arr| arr.iter().map(|a| a["login"].clone()).collect::<Vec<_>>()),
                    "created_at": i["created_at"],
                    "updated_at": i["updated_at"],
                    "comments": i["comments"],
                })
            })
            .collect();

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&shaped)
                .unwrap_or_else(|_| serde_json::to_string(&shaped).unwrap_or_default()),
            error: None,
        })
    }

    async fn get_issue(
        &self,
        owner: &str,
        repo: &str,
        issue_number: u32,
    ) -> anyhow::Result<ToolResult> {
        let url = format!(
            "{}/repos/{}/{}/issues/{}",
            self.api_url, owner, repo, issue_number
        );
        let resp = self
            .http
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github.v3+json")
            .timeout(std::time::Duration::from_secs(self.timeout_secs))
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("GitHub get_issue request failed: {e}"))?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!(
                "GitHub get_issue failed ({status}): {}",
                crate::util::truncate_with_ellipsis(&text, MAX_ERROR_BODY_CHARS)
            );
        }

        let raw: Value = resp
            .json()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to parse GitHub get_issue response: {e}"))?;

        let shaped = json!({
            "number": raw["number"],
            "title": raw["title"],
            "state": raw["state"],
            "body": raw["body"],
            "html_url": raw["html_url"],
            "user": raw["user"]["login"],
            "labels": raw["labels"].as_array().map(|arr| arr.iter().map(|l| l["name"].clone()).collect::<Vec<_>>()),
            "assignees": raw["assignees"].as_array().map(|arr| arr.iter().map(|a| a["login"].clone()).collect::<Vec<_>>()),
            "created_at": raw["created_at"],
            "updated_at": raw["updated_at"],
            "closed_at": raw["closed_at"],
            "comments": raw["comments"],
            "milestone": raw["milestone"]["title"],
        });

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&shaped)
                .unwrap_or_else(|_| serde_json::to_string(&shaped).unwrap_or_default()),
            error: None,
        })
    }

    async fn create_issue(
        &self,
        owner: &str,
        repo: &str,
        title: &str,
        body: Option<&str>,
        labels: Option<Vec<String>>,
        assignees: Option<Vec<String>>,
    ) -> anyhow::Result<ToolResult> {
        let url = format!("{}/repos/{}/{}/issues", self.api_url, owner, repo);

        let mut payload = json!({
            "title": title,
        });

        if let Some(b) = body {
            payload["body"] = json!(b);
        }
        if let Some(l) = labels {
            payload["labels"] = json!(l);
        }
        if let Some(a) = assignees {
            payload["assignees"] = json!(a);
        }

        let resp = self
            .http
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github.v3+json")
            .header("Content-Type", "application/json")
            .json(&payload)
            .timeout(std::time::Duration::from_secs(self.timeout_secs))
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("GitHub create_issue request failed: {e}"))?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!(
                "GitHub create_issue failed ({status}): {}",
                crate::util::truncate_with_ellipsis(&text, MAX_ERROR_BODY_CHARS)
            );
        }

        let raw: Value = resp
            .json()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to parse GitHub create_issue response: {e}"))?;

        let shaped = json!({
            "number": raw["number"],
            "title": raw["title"],
            "html_url": raw["html_url"],
            "state": raw["state"],
        });

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&shaped)
                .unwrap_or_else(|_| serde_json::to_string(&shaped).unwrap_or_default()),
            error: None,
        })
    }

    async fn comment_issue(
        &self,
        owner: &str,
        repo: &str,
        issue_number: u32,
        body: &str,
    ) -> anyhow::Result<ToolResult> {
        let url = format!(
            "{}/repos/{}/{}/issues/{}/comments",
            self.api_url, owner, repo, issue_number
        );

        let resp = self
            .http
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github.v3+json")
            .header("Content-Type", "application/json")
            .json(&json!({ "body": body }))
            .timeout(std::time::Duration::from_secs(self.timeout_secs))
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("GitHub comment_issue request failed: {e}"))?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!(
                "GitHub comment_issue failed ({status}): {}",
                crate::util::truncate_with_ellipsis(&text, MAX_ERROR_BODY_CHARS)
            );
        }

        let raw: Value = resp
            .json()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to parse GitHub comment_issue response: {e}"))?;

        let shaped = json!({
            "id": raw["id"],
            "html_url": raw["html_url"],
            "user": raw["user"]["login"],
            "created_at": raw["created_at"],
        });

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&shaped)
                .unwrap_or_else(|_| serde_json::to_string(&shaped).unwrap_or_default()),
            error: None,
        })
    }

    async fn list_prs(
        &self,
        owner: &str,
        repo: &str,
        state: Option<&str>,
    ) -> anyhow::Result<ToolResult> {
        let url = format!("{}/repos/{}/{}/pulls", self.api_url, owner, repo);
        let query: Vec<(&str, &str)> = vec![
            ("state", state.unwrap_or("open")),
            ("sort", "updated"),
            ("direction", "desc"),
            ("per_page", "30"),
        ];

        let resp = self
            .http
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github.v3+json")
            .query(&query)
            .timeout(std::time::Duration::from_secs(self.timeout_secs))
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("GitHub list_prs request failed: {e}"))?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!(
                "GitHub list_prs failed ({status}): {}",
                crate::util::truncate_with_ellipsis(&text, MAX_ERROR_BODY_CHARS)
            );
        }

        let prs: Vec<Value> = resp
            .json()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to parse GitHub list_prs response: {e}"))?;

        let shaped: Vec<Value> = prs
            .iter()
            .map(|p| {
                json!({
                    "number": p["number"],
                    "title": p["title"],
                    "state": p["state"],
                    "html_url": p["html_url"],
                    "user": p["user"]["login"],
                    "head": {
                        "ref": p["head"]["ref"],
                        "repo": p["head"]["repo"]["name"],
                    },
                    "base": {
                        "ref": p["base"]["ref"],
                        "repo": p["base"]["repo"]["name"],
                    },
                    "draft": p["draft"],
                    "mergeable": p["mergeable"],
                    "merged": p["merged"],
                    "commits": p["commits"],
                    "additions": p["additions"],
                    "deletions": p["deletions"],
                    "changed_files": p["changed_files"],
                    "created_at": p["created_at"],
                    "updated_at": p["updated_at"],
                })
            })
            .collect();

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&shaped)
                .unwrap_or_else(|_| serde_json::to_string(&shaped).unwrap_or_default()),
            error: None,
        })
    }

    async fn get_pr(&self, owner: &str, repo: &str, pr_number: u32) -> anyhow::Result<ToolResult> {
        let url = format!(
            "{}/repos/{}/{}/pulls/{}",
            self.api_url, owner, repo, pr_number
        );
        let resp = self
            .http
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github.v3+json")
            .timeout(std::time::Duration::from_secs(self.timeout_secs))
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("GitHub get_pr request failed: {e}"))?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!(
                "GitHub get_pr failed ({status}): {}",
                crate::util::truncate_with_ellipsis(&text, MAX_ERROR_BODY_CHARS)
            );
        }

        let raw: Value = resp
            .json()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to parse GitHub get_pr response: {e}"))?;

        let shaped = json!({
            "number": raw["number"],
            "title": raw["title"],
            "body": raw["body"],
            "state": raw["state"],
            "html_url": raw["html_url"],
            "user": raw["user"]["login"],
            "draft": raw["draft"],
            "mergeable": raw["mergeable"],
            "mergeable_state": raw["mergeable_state"],
            "merged": raw["merged"],
            "commits": raw["commits"],
            "additions": raw["additions"],
            "deletions": raw["deletions"],
            "changed_files": raw["changed_files"],
            "head": {
                "ref": raw["head"]["ref"],
                "sha": raw["head"]["sha"],
            },
            "base": {
                "ref": raw["base"]["ref"],
                "sha": raw["base"]["sha"],
            },
            "reviewers": raw["requested_reviewers"].as_array().map(|arr| arr.iter().map(|r| r["login"].clone()).collect::<Vec<_>>()),
            "labels": raw["labels"].as_array().map(|arr| arr.iter().map(|l| l["name"].clone()).collect::<Vec<_>>()),
            "created_at": raw["created_at"],
            "updated_at": raw["updated_at"],
        });

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&shaped)
                .unwrap_or_else(|_| serde_json::to_string(&shaped).unwrap_or_default()),
            error: None,
        })
    }

    async fn merge_pr(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u32,
        commit_title: Option<&str>,
        commit_message: Option<&str>,
        merge_method: Option<&str>,
    ) -> anyhow::Result<ToolResult> {
        let url = format!(
            "{}/repos/{}/{}/pulls/{}/merge",
            self.api_url, owner, repo, pr_number
        );

        let mut payload = json!({});
        if let Some(t) = commit_title {
            payload["commit_title"] = json!(t);
        }
        if let Some(m) = commit_message {
            payload["commit_message"] = json!(m);
        }
        if let Some(mm) = merge_method {
            payload["merge_method"] = json!(mm);
        }

        let resp = self
            .http
            .put(&url)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github.v3+json")
            .header("Content-Type", "application/json")
            .json(&payload)
            .timeout(std::time::Duration::from_secs(self.timeout_secs))
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("GitHub merge_pr request failed: {e}"))?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!(
                "GitHub merge_pr failed ({status}): {}",
                crate::util::truncate_with_ellipsis(&text, MAX_ERROR_BODY_CHARS)
            );
        }

        let raw: Value = resp
            .json()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to parse GitHub merge_pr response: {e}"))?;

        let shaped = json!({
            "merged": raw["merged"],
            "merge_commit_sha": raw["merge_commit_sha"],
            "message": raw["message"],
        });

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&shaped)
                .unwrap_or_else(|_| serde_json::to_string(&shaped).unwrap_or_default()),
            error: None,
        })
    }

    async fn list_workflows(&self, owner: &str, repo: &str) -> anyhow::Result<ToolResult> {
        let url = format!(
            "{}/repos/{}/{}/actions/workflows",
            self.api_url, owner, repo
        );

        let resp = self
            .http
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github.v3+json")
            .timeout(std::time::Duration::from_secs(self.timeout_secs))
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("GitHub list_workflows request failed: {e}"))?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!(
                "GitHub list_workflows failed ({status}): {}",
                crate::util::truncate_with_ellipsis(&text, MAX_ERROR_BODY_CHARS)
            );
        }

        let raw: Value = resp
            .json()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to parse GitHub list_workflows response: {e}"))?;

        let workflows = raw["workflows"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .map(|w| {
                        json!({
                            "id": w["id"],
                            "name": w["name"],
                            "path": w["path"],
                            "state": w["state"],
                            "html_url": w["html_url"],
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&workflows)
                .unwrap_or_else(|_| serde_json::to_string(&workflows).unwrap_or_default()),
            error: None,
        })
    }

    async fn list_runs(
        &self,
        owner: &str,
        repo: &str,
        workflow_id: Option<&str>,
        branch: Option<&str>,
    ) -> anyhow::Result<ToolResult> {
        let url = if let Some(wid) = workflow_id {
            format!(
                "{}/repos/{}/{}/actions/workflows/{}/runs",
                self.api_url, owner, repo, wid
            )
        } else {
            format!("{}/repos/{}/{}/actions/runs", self.api_url, owner, repo)
        };

        let mut query: Vec<(&str, &str)> = vec![];
        if let Some(b) = branch {
            query.push(("branch", b));
        }

        let resp = self
            .http
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github.v3+json")
            .query(&query)
            .timeout(std::time::Duration::from_secs(self.timeout_secs))
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("GitHub list_runs request failed: {e}"))?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!(
                "GitHub list_runs failed ({status}): {}",
                crate::util::truncate_with_ellipsis(&text, MAX_ERROR_BODY_CHARS)
            );
        }

        let raw: Value = resp
            .json()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to parse GitHub list_runs response: {e}"))?;

        let runs = raw["workflow_runs"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .take(30)
                    .map(|r| {
                        json!({
                            "id": r["id"],
                            "name": r["name"],
                            "head_branch": r["head_branch"],
                            "head_sha": r["head_sha"],
                            "status": r["status"],
                            "conclusion": r["conclusion"],
                            "html_url": r["html_url"],
                            "actor": r["actor"]["login"],
                            "event": r["event"],
                            "run_number": r["run_number"],
                            "run_attempt": r["run_attempt"],
                            "created_at": r["created_at"],
                            "updated_at": r["updated_at"],
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&runs)
                .unwrap_or_else(|_| serde_json::to_string(&runs).unwrap_or_default()),
            error: None,
        })
    }

    async fn trigger_workflow(
        &self,
        owner: &str,
        repo: &str,
        workflow_id: &str,
        ref_: &str,
        inputs: Option<Value>,
    ) -> anyhow::Result<ToolResult> {
        let url = format!(
            "{}/repos/{}/{}/actions/workflows/{}/dispatches",
            self.api_url, owner, repo, workflow_id
        );

        let mut payload = json!({
            "ref": ref_,
        });
        if let Some(i) = inputs {
            payload["inputs"] = i;
        }

        let resp = self
            .http
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github.v3+json")
            .header("Content-Type", "application/json")
            .json(&payload)
            .timeout(std::time::Duration::from_secs(self.timeout_secs))
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("GitHub trigger_workflow request failed: {e}"))?;

        let status = resp.status();
        if status.as_u16() != 204 {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!(
                "GitHub trigger_workflow failed ({status}): {}",
                crate::util::truncate_with_ellipsis(&text, MAX_ERROR_BODY_CHARS)
            );
        }

        Ok(ToolResult {
            success: true,
            output: format!(
                "Workflow '{}' triggered successfully on branch/tag '{}'",
                workflow_id, ref_
            ),
            error: None,
        })
    }
}

#[async_trait]
impl Tool for GitHubTool {
    fn name(&self) -> &str {
        "github"
    }

    fn description(&self) -> &str {
        "Interact with GitHub: manage repositories, issues, pull requests, and GitHub Actions workflows."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        let read_actions = ["get_repo", "list_repos", "list_issues", "get_issue"];
        let write_actions = [
            "create_issue",
            "comment_issue",
            "create_pr",
            "merge_pr",
            "trigger_workflow",
        ];
        let all_actions: Vec<&str> = read_actions
            .iter()
            .chain(write_actions.iter())
            .chain(["list_prs", "get_pr", "list_workflows", "list_runs"].iter())
            .copied()
            .collect();

        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": all_actions,
                    "description": "The GitHub action to perform. Read actions (get_repo, list_repos, list_issues, get_issue) are enabled by default. Write actions require explicit configuration in [github].allowed_actions."
                },
                "owner": {
                    "type": "string",
                    "description": "Repository owner (user or organization). Optional if default_owner is configured."
                },
                "repo": {
                    "type": "string",
                    "description": "Repository name. Required for most actions."
                },
                "issue_number": {
                    "type": "integer",
                    "description": "Issue or PR number. Required for get_issue, comment_issue, get_pr, merge_pr."
                },
                "pr_number": {
                    "type": "integer",
                    "description": "PR number. Alias for issue_number when working with pull requests."
                },
                "title": {
                    "type": "string",
                    "description": "Issue or PR title. Required for create_issue."
                },
                "body": {
                    "type": "string",
                    "description": "Issue body, PR description, or comment text."
                },
                "state": {
                    "type": "string",
                    "enum": ["open", "closed", "all"],
                    "description": "Filter by state: 'open', 'closed', or 'all'. Defaults to 'open' for issues and PRs."
                },
                "labels": {
                    "type": "string",
                    "description": "Comma-separated labels to filter by or apply. For create_issue, provide a JSON array string like '[ \"bug\", \"priority\" ]'."
                },
                "assignees": {
                    "type": "string",
                    "description": "Comma-separated usernames to assign. For create_issue, provide a JSON array string."
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum number of results for list operations. Defaults to 25, capped at 100."
                },
                "workflow_id": {
                    "type": "string",
                    "description": "Workflow ID (numeric) or filename (e.g., 'ci.yml'). Required for list_runs and trigger_workflow."
                },
                "branch": {
                    "type": "string",
                    "description": "Branch name for list_runs or ref for trigger_workflow."
                },
                "ref": {
                    "type": "string",
                    "description": "Git ref (branch, tag, or commit SHA) to trigger workflow on. Defaults to 'main'."
                },
                "merge_method": {
                    "type": "string",
                    "enum": ["merge", "squash", "rebase"],
                    "description": "Merge method for merge_pr. Defaults to 'merge'."
                },
                "inputs": {
                    "type": "object",
                    "description": "Workflow dispatch inputs as key-value pairs. Example: { \"environment\": \"staging\" }"
                },
                "type": {
                    "type": "string",
                    "enum": ["all", "owner", "public", "private", "sources", "forks"],
                    "description": "Repository type filter for list_repos. Defaults to 'owner'."
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let action = match args["action"].as_str() {
            Some(a) => a,
            None => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("Missing required field: action".to_string()),
                });
            }
        };

        if !self.is_action_allowed(action) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Action '{}' is not allowed. Configure it in [github].allowed_actions to enable.",
                    action
                )),
            });
        }

        #[allow(clippy::match_same_arms)]
        let security_op = match action {
            "get_repo" | "list_repos" | "list_issues" | "get_issue" | "list_prs" | "get_pr"
            | "list_workflows" | "list_runs" => ToolOperation::Read,
            "create_issue" | "comment_issue" | "create_pr" | "merge_pr" | "trigger_workflow" => {
                ToolOperation::Act
            }
            _ => ToolOperation::Read,
        };

        if let Err(error) = self.security.enforce_tool_operation(security_op, "github") {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(error),
            });
        }

        let owner = args["owner"].as_str();
        let repo = args["repo"].as_str();

        match action {
            "get_repo" => {
                let owner = owner.unwrap_or_default();
                let repo = repo.unwrap_or_default();
                if owner.is_empty() || repo.is_empty() {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some("owner and repo are required for get_repo".to_string()),
                    });
                }
                Ok(self
                    .get_repo(owner, repo)
                    .await
                    .unwrap_or_else(|e| ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(e.to_string()),
                    }))
            }

            "list_repos" => Ok(self
                .list_repos(owner, args["type"].as_str())
                .await
                .unwrap_or_else(|e| ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(e.to_string()),
                })),

            "list_issues" => {
                let owner = owner.unwrap_or_default();
                let repo = repo.unwrap_or_default();
                if owner.is_empty() || repo.is_empty() {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some("owner and repo are required for list_issues".to_string()),
                    });
                }
                let labels = args["labels"].as_str().filter(|s| !s.is_empty());
                let state = args["state"].as_str();
                let max_results = args["max_results"]
                    .as_u64()
                    .and_then(|n| u32::try_from(n).ok());

                Ok(self
                    .list_issues(owner, repo, state, labels, max_results)
                    .await
                    .unwrap_or_else(|e| ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(e.to_string()),
                    }))
            }

            "get_issue" => {
                let owner = owner.unwrap_or_default();
                let repo = repo.unwrap_or_default();
                let issue_number =
                    u32::try_from(args["issue_number"].as_u64().unwrap_or(0)).unwrap_or(0);
                if owner.is_empty() || repo.is_empty() || issue_number == 0 {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(
                            "owner, repo, and issue_number are required for get_issue".to_string(),
                        ),
                    });
                }
                Ok(self
                    .get_issue(owner, repo, issue_number)
                    .await
                    .unwrap_or_else(|e| ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(e.to_string()),
                    }))
            }

            "create_issue" => {
                let owner = owner.unwrap_or_default();
                let repo = repo.unwrap_or_default();
                let title = args["title"].as_str().unwrap_or_default();
                if owner.is_empty() || repo.is_empty() || title.is_empty() {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(
                            "owner, repo, and title are required for create_issue".to_string(),
                        ),
                    });
                }

                let body = args["body"].as_str().filter(|s| !s.is_empty());
                let labels = args["labels"]
                    .as_str()
                    .and_then(|s| serde_json::from_str(s).ok());
                let assignees = args["assignees"]
                    .as_str()
                    .and_then(|s| serde_json::from_str(s).ok());

                Ok(self
                    .create_issue(owner, repo, title, body, labels, assignees)
                    .await
                    .unwrap_or_else(|e| ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(e.to_string()),
                    }))
            }

            "comment_issue" => {
                let owner = owner.unwrap_or_default();
                let repo = repo.unwrap_or_default();
                let issue_number =
                    u32::try_from(args["issue_number"].as_u64().unwrap_or(0)).unwrap_or(0);
                let body = args["body"].as_str().unwrap_or_default();
                if owner.is_empty() || repo.is_empty() || issue_number == 0 || body.is_empty() {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(
                            "owner, repo, issue_number, and body are required for comment_issue"
                                .to_string(),
                        ),
                    });
                }
                Ok(self
                    .comment_issue(owner, repo, issue_number, body)
                    .await
                    .unwrap_or_else(|e| ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(e.to_string()),
                    }))
            }

            "list_prs" => {
                let owner = owner.unwrap_or_default();
                let repo = repo.unwrap_or_default();
                if owner.is_empty() || repo.is_empty() {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some("owner and repo are required for list_prs".to_string()),
                    });
                }
                let state = args["state"].as_str();
                Ok(self
                    .list_prs(owner, repo, state)
                    .await
                    .unwrap_or_else(|e| ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(e.to_string()),
                    }))
            }

            "get_pr" => {
                let owner = owner.unwrap_or_default();
                let repo = repo.unwrap_or_default();
                let pr_number = u32::try_from(
                    args["pr_number"]
                        .as_u64()
                        .or(args["issue_number"].as_u64())
                        .unwrap_or(0),
                )
                .unwrap_or(0);
                if owner.is_empty() || repo.is_empty() || pr_number == 0 {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(
                            "owner, repo, and pr_number are required for get_pr".to_string(),
                        ),
                    });
                }
                Ok(self
                    .get_pr(owner, repo, pr_number)
                    .await
                    .unwrap_or_else(|e| ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(e.to_string()),
                    }))
            }

            "merge_pr" => {
                let owner = owner.unwrap_or_default();
                let repo = repo.unwrap_or_default();
                let pr_number = u32::try_from(
                    args["pr_number"]
                        .as_u64()
                        .or(args["issue_number"].as_u64())
                        .unwrap_or(0),
                )
                .unwrap_or(0);
                if owner.is_empty() || repo.is_empty() || pr_number == 0 {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(
                            "owner, repo, and pr_number are required for merge_pr".to_string(),
                        ),
                    });
                }
                let commit_title = args["commit_title"].as_str().filter(|s| !s.is_empty());
                let commit_message = args["commit_message"].as_str().filter(|s| !s.is_empty());
                let merge_method = args["merge_method"].as_str().filter(|s| !s.is_empty());

                Ok(self
                    .merge_pr(
                        owner,
                        repo,
                        pr_number,
                        commit_title,
                        commit_message,
                        merge_method,
                    )
                    .await
                    .unwrap_or_else(|e| ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(e.to_string()),
                    }))
            }

            "list_workflows" => {
                let owner = owner.unwrap_or_default();
                let repo = repo.unwrap_or_default();
                if owner.is_empty() || repo.is_empty() {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some("owner and repo are required for list_workflows".to_string()),
                    });
                }
                Ok(self
                    .list_workflows(owner, repo)
                    .await
                    .unwrap_or_else(|e| ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(e.to_string()),
                    }))
            }

            "list_runs" => {
                let owner = owner.unwrap_or_default();
                let repo = repo.unwrap_or_default();
                if owner.is_empty() || repo.is_empty() {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some("owner and repo are required for list_runs".to_string()),
                    });
                }
                let workflow_id = args["workflow_id"].as_str().filter(|s| !s.is_empty());
                let branch = args["branch"].as_str().filter(|s| !s.is_empty());

                Ok(self
                    .list_runs(owner, repo, workflow_id, branch)
                    .await
                    .unwrap_or_else(|e| ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(e.to_string()),
                    }))
            }

            "trigger_workflow" => {
                let owner = owner.unwrap_or_default();
                let repo = repo.unwrap_or_default();
                let workflow_id = args["workflow_id"].as_str().unwrap_or_default();
                if owner.is_empty() || repo.is_empty() || workflow_id.is_empty() {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(
                            "owner, repo, and workflow_id are required for trigger_workflow"
                                .to_string(),
                        ),
                    });
                }
                let ref_ = args["ref"].as_str().unwrap_or("main");
                let inputs = args["inputs"]
                    .as_object()
                    .map(|m| serde_json::Value::Object(m.clone()));

                Ok(self
                    .trigger_workflow(owner, repo, workflow_id, ref_, inputs)
                    .await
                    .unwrap_or_else(|e| ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(e.to_string()),
                    }))
            }

            _ => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Unknown action: {}", action)),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_action_allowed() {
        let config = GitHubConfig {
            enabled: true,
            token: "test".to_string(),
            api_url: "https://api.github.com".to_string(),
            allowed_actions: vec!["get_repo".to_string(), "list_issues".to_string()],
            timeout_secs: 30,
            default_owner: None,
        };
        let security = Arc::new(SecurityPolicy::default());
        let tool = GitHubTool::new(config, security);

        assert!(tool.is_action_allowed("get_repo"));
        assert!(tool.is_action_allowed("list_issues"));
        assert!(!tool.is_action_allowed("create_issue"));
    }

    #[test]
    fn test_resolve_owner() {
        let config = GitHubConfig {
            enabled: true,
            token: "test".to_string(),
            api_url: "https://api.github.com".to_string(),
            allowed_actions: vec!["get_repo".to_string()],
            timeout_secs: 30,
            default_owner: Some("default-owner".to_string()),
        };
        let security = Arc::new(SecurityPolicy::default());
        let tool = GitHubTool::new(config, security);

        assert_eq!(
            tool.resolve_owner(Some("explicit-owner")),
            Some("explicit-owner".to_string())
        );
        assert_eq!(tool.resolve_owner(None), Some("default-owner".to_string()));
    }

    #[test]
    fn test_tool_name() {
        let config = GitHubConfig::default();
        let security = Arc::new(SecurityPolicy::default());
        let tool = GitHubTool::new(config, security);
        assert_eq!(tool.name(), "github");
    }

    #[test]
    fn test_tool_description() {
        let config = GitHubConfig::default();
        let security = Arc::new(SecurityPolicy::default());
        let tool = GitHubTool::new(config, security);
        assert!(!tool.description().is_empty());
    }

    #[test]
    fn test_parameters_schema() {
        let config = GitHubConfig::default();
        let security = Arc::new(SecurityPolicy::default());
        let tool = GitHubTool::new(config, security);
        let schema = tool.parameters_schema();

        assert_eq!(schema["type"], "object");
        assert!(schema["properties"].as_object().is_some());
        assert!(schema["properties"]["action"].as_object().is_some());
    }

    #[tokio::test]
    async fn test_get_repo_success() {
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;
        let repo_response = json!({
            "name": "test-repo",
            "full_name": "test-owner/test-repo",
            "description": "A test repository",
            "private": false,
            "html_url": "https://github.com/test-owner/test-repo",
            "default_branch": "main",
            "stargazers_count": 100,
            "forks_count": 10,
            "open_issues_count": 5,
            "language": "Rust",
            "pushed_at": "2024-01-01T00:00:00Z",
            "topics": ["rust", "testing"]
        });

        Mock::given(method("GET"))
            .and(path("/repos/test-owner/test-repo"))
            .and(header("Authorization", "Bearer test-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&repo_response))
            .mount(&mock_server)
            .await;

        let config = GitHubConfig {
            enabled: true,
            token: "test-token".to_string(),
            api_url: mock_server.uri(),
            allowed_actions: vec!["get_repo".to_string()],
            timeout_secs: 30,
            default_owner: None,
        };
        let security = Arc::new(SecurityPolicy::default());
        let tool = GitHubTool::new(config, security);

        let result = tool
            .execute(json!({
                "action": "get_repo",
                "owner": "test-owner",
                "repo": "test-repo"
            }))
            .await
            .unwrap();

        assert!(
            result.success,
            "get_repo should succeed: {:?}",
            result.error
        );
        let output: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(output["name"], "test-repo");
        assert_eq!(output["full_name"], "test-owner/test-repo");
        assert_eq!(output["stargazers_count"], 100);
    }

    #[tokio::test]
    async fn test_get_repo_not_found() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/repos/nonexistent/repo"))
            .respond_with(ResponseTemplate::new(404).set_body_json(json!({
                "message": "Not Found",
                "documentation_url": "https://docs.github.com/rest/repos/repos#get-a-repository"
            })))
            .mount(&mock_server)
            .await;

        let config = GitHubConfig {
            enabled: true,
            token: "test-token".to_string(),
            api_url: mock_server.uri(),
            allowed_actions: vec!["get_repo".to_string()],
            timeout_secs: 30,
            default_owner: None,
        };
        let security = Arc::new(SecurityPolicy::default());
        let tool = GitHubTool::new(config, security);

        let result = tool
            .execute(json!({
                "action": "get_repo",
                "owner": "nonexistent",
                "repo": "repo"
            }))
            .await
            .expect("execute should return Ok result");

        assert!(
            !result.success,
            "get_repo should fail for non-existent repo"
        );
        assert!(result.error.unwrap().contains("404"));
    }

    #[tokio::test]
    async fn test_action_not_allowed() {
        let config = GitHubConfig {
            enabled: true,
            token: "test-token".to_string(),
            api_url: "https://api.github.com".to_string(),
            allowed_actions: vec!["get_repo".to_string()], // create_issue NOT allowed
            timeout_secs: 30,
            default_owner: None,
        };
        let security = Arc::new(SecurityPolicy::default());
        let tool = GitHubTool::new(config, security);

        let result = tool
            .execute(json!({
                "action": "create_issue",
                "owner": "test-owner",
                "repo": "test-repo",
                "title": "Test Issue"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.unwrap().contains("not allowed"));
    }

    #[tokio::test]
    async fn test_missing_required_params() {
        let config = GitHubConfig {
            enabled: true,
            token: "test-token".to_string(),
            api_url: "https://api.github.com".to_string(),
            allowed_actions: vec!["get_repo".to_string(), "list_issues".to_string()],
            timeout_secs: 30,
            default_owner: None,
        };
        let security = Arc::new(SecurityPolicy::default());
        let tool = GitHubTool::new(config, security);

        // Missing owner and repo
        let result = tool.execute(json!({"action": "get_repo"})).await.unwrap();

        assert!(!result.success);
        assert!(result
            .error
            .unwrap()
            .contains("owner and repo are required"));
    }

    #[tokio::test]
    async fn test_list_issues_success() {
        use wiremock::matchers::{header, method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;
        let issues_response = json!([
            {
                "number": 1,
                "title": "Bug: Something is broken",
                "state": "open",
                "html_url": "https://github.com/test-owner/test-repo/issues/1",
                "user": {"login": "testuser"},
                "labels": [{"name": "bug"}, {"name": "priority"}],
                "assignees": [{"login": "contributor"}],
                "created_at": "2024-01-01T00:00:00Z",
                "updated_at": "2024-01-02T00:00:00Z",
                "comments": 5
            },
            {
                "number": 2,
                "title": "Feature: Add new capability",
                "state": "closed",
                "html_url": "https://github.com/test-owner/test-repo/issues/2",
                "user": {"login": "anotheruser"},
                "labels": [{"name": "enhancement"}],
                "assignees": [],
                "created_at": "2024-01-03T00:00:00Z",
                "updated_at": "2024-01-04T00:00:00Z",
                "comments": 0
            }
        ]);

        Mock::given(method("GET"))
            .and(path("/repos/test-owner/test-repo/issues"))
            .and(header("Authorization", "Bearer test-token"))
            .and(query_param("state", "open"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&issues_response))
            .mount(&mock_server)
            .await;

        let config = GitHubConfig {
            enabled: true,
            token: "test-token".to_string(),
            api_url: mock_server.uri(),
            allowed_actions: vec!["list_issues".to_string()],
            timeout_secs: 30,
            default_owner: None,
        };
        let security = Arc::new(SecurityPolicy::default());
        let tool = GitHubTool::new(config, security);

        let result = tool
            .execute(json!({
                "action": "list_issues",
                "owner": "test-owner",
                "repo": "test-repo",
                "state": "open"
            }))
            .await
            .unwrap();

        assert!(
            result.success,
            "list_issues should succeed: {:?}",
            result.error
        );
        let output: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(output.as_array().unwrap().len(), 2);
        assert_eq!(output[0]["number"], 1);
        assert_eq!(output[0]["title"], "Bug: Something is broken");
    }

    #[tokio::test]
    async fn test_create_issue_success() {
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;
        let create_response = json!({
            "number": 42,
            "title": "New Issue Title",
            "html_url": "https://github.com/test-owner/test-repo/issues/42",
            "state": "open"
        });

        Mock::given(method("POST"))
            .and(path("/repos/test-owner/test-repo/issues"))
            .and(header("Authorization", "Bearer test-token"))
            .and(header("Content-Type", "application/json"))
            .respond_with(ResponseTemplate::new(201).set_body_json(&create_response))
            .mount(&mock_server)
            .await;

        let config = GitHubConfig {
            enabled: true,
            token: "test-token".to_string(),
            api_url: mock_server.uri(),
            allowed_actions: vec!["create_issue".to_string()],
            timeout_secs: 30,
            default_owner: None,
        };
        let security = Arc::new(SecurityPolicy::default());
        let tool = GitHubTool::new(config, security);

        let result = tool
            .execute(json!({
                "action": "create_issue",
                "owner": "test-owner",
                "repo": "test-repo",
                "title": "New Issue Title",
                "body": "Issue description"
            }))
            .await
            .unwrap();

        assert!(
            result.success,
            "create_issue should succeed: {:?}",
            result.error
        );
        let output: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(output["number"], 42);
        assert_eq!(output["title"], "New Issue Title");
    }

    #[tokio::test]
    async fn test_unknown_action() {
        let config = GitHubConfig {
            enabled: true,
            token: "test-token".to_string(),
            api_url: "https://api.github.com".to_string(),
            allowed_actions: vec!["get_repo".to_string(), "unknown_action".to_string()],
            timeout_secs: 30,
            default_owner: None,
        };
        let security = Arc::new(SecurityPolicy::default());
        let tool = GitHubTool::new(config, security);

        let result = tool
            .execute(json!({"action": "unknown_action"}))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.unwrap().contains("Unknown action"));
    }
}
