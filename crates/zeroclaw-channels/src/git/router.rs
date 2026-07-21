//! Per-event routing policy: resolves an event type against the
//! `[channels.git.<alias>.events]` table and derives which API endpoint
//! families to poll from it. Pure functions over config — no IO, no
//! payload knowledge, no forge specifics.

use std::collections::HashMap;

use zeroclaw_config::schema::GitEventRoute;

use super::types::{
    EVT_ISSUE_COMMENT_CREATED, EVT_ISSUES_OPENED, EVT_PR_REVIEW_COMMENT_CREATED,
    EVT_PULL_REQUEST_CLOSED, EVT_PULL_REQUEST_MERGED, EVT_PULL_REQUEST_OPENED,
    EVT_RELEASE_PUBLISHED, EVT_WORKFLOW_RUN_COMPLETED, EVT_WORKFLOW_RUN_FAILED, KNOWN_EVENT_TYPES,
};

/// What the routing table says to do with one event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteAction {
    /// Not routed: drop silently.
    Ignore,
    /// Deliver as a channel message with the conversational filters
    /// (mention gate under `mention_only`).
    Message,
    /// Emit a channel-sourced SOP event.
    Sop { sop: String },
}

pub fn resolve_route(event_type: &str, table: &HashMap<String, GitEventRoute>) -> RouteAction {
    match table.get(event_type) {
        Some(route) => {
            if let Some(sop) = route
                .sop
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                RouteAction::Sop {
                    sop: sop.to_string(),
                }
            } else if route.message {
                RouteAction::Message
            } else {
                RouteAction::Ignore
            }
        }
        None => match event_type {
            EVT_ISSUE_COMMENT_CREATED | EVT_ISSUES_OPENED | EVT_PULL_REQUEST_OPENED => {
                RouteAction::Message
            }
            _ => RouteAction::Ignore,
        },
    }
}

/// Routing-table entries that can never fire: unknown event-type keys
/// (typos) and `sop = ""` entries. Surfaced as startup warnings.
pub fn validate_routes(table: &HashMap<String, GitEventRoute>) -> Vec<String> {
    let mut problems = Vec::new();
    for (key, route) in table {
        if !KNOWN_EVENT_TYPES.contains(&key.as_str()) {
            problems.push(format!(
                "unknown event type `{key}` (known: {})",
                KNOWN_EVENT_TYPES.join(", ")
            ));
        }
        if route.sop.as_deref().is_some_and(|s| s.trim().is_empty()) {
            problems.push(format!("event `{key}` has an empty `sop` name"));
        }
    }
    problems.sort();
    problems
}

/// Which API endpoint families to poll, derived from the effective
/// routing table — routing an event type is subscribing to it, so there
/// is no separate subscription list to drift. The feed backbone is a
/// config toggle, not part of this plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransportPlan {
    /// Opening posts and PR close/merge transitions.
    pub issues: bool,
    /// Issue/PR comments.
    pub comments: bool,
    /// Inline review comments.
    pub review_comments: bool,
    /// Releases.
    pub releases: bool,
    /// Workflow/pipeline runs.
    pub workflow_runs: bool,
}

impl TransportPlan {
    pub fn from_routes(table: &HashMap<String, GitEventRoute>) -> Self {
        let routed = |t: &str| resolve_route(t, table) != RouteAction::Ignore;
        Self {
            issues: routed(EVT_ISSUES_OPENED)
                || routed(EVT_PULL_REQUEST_OPENED)
                || routed(EVT_PULL_REQUEST_CLOSED)
                || routed(EVT_PULL_REQUEST_MERGED),
            comments: routed(EVT_ISSUE_COMMENT_CREATED),
            review_comments: routed(EVT_PR_REVIEW_COMMENT_CREATED),
            releases: routed(EVT_RELEASE_PUBLISHED),
            workflow_runs: routed(EVT_WORKFLOW_RUN_COMPLETED) || routed(EVT_WORKFLOW_RUN_FAILED),
        }
    }

    /// Whether anything at all is subscribed.
    pub fn any(&self) -> bool {
        self.issues || self.comments || self.review_comments || self.releases || self.workflow_runs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(message: bool, sop: Option<&str>) -> GitEventRoute {
        GitEventRoute {
            message,
            sop: sop.map(str::to_string),
        }
    }

    #[test]
    fn defaults_route_the_conversational_trio_only() {
        let table = HashMap::new();
        assert_eq!(
            resolve_route(EVT_ISSUE_COMMENT_CREATED, &table),
            RouteAction::Message
        );
        assert_eq!(
            resolve_route(EVT_ISSUES_OPENED, &table),
            RouteAction::Message
        );
        assert_eq!(
            resolve_route(EVT_PULL_REQUEST_OPENED, &table),
            RouteAction::Message
        );
        for ignored in [
            EVT_PULL_REQUEST_CLOSED,
            EVT_PULL_REQUEST_MERGED,
            EVT_PR_REVIEW_COMMENT_CREATED,
            EVT_WORKFLOW_RUN_COMPLETED,
            EVT_WORKFLOW_RUN_FAILED,
            EVT_RELEASE_PUBLISHED,
        ] {
            assert_eq!(
                resolve_route(ignored, &table),
                RouteAction::Ignore,
                "{ignored}"
            );
        }
    }

    #[test]
    fn explicit_entries_override_defaults_either_way() {
        let mut table = HashMap::new();
        // Opting OUT of a default…
        table.insert(EVT_ISSUE_COMMENT_CREATED.to_string(), route(false, None));
        // …and opting IN to a non-default.
        table.insert(EVT_WORKFLOW_RUN_FAILED.to_string(), route(true, None));
        assert_eq!(
            resolve_route(EVT_ISSUE_COMMENT_CREATED, &table),
            RouteAction::Ignore
        );
        assert_eq!(
            resolve_route(EVT_WORKFLOW_RUN_FAILED, &table),
            RouteAction::Message
        );
    }

    #[test]
    fn sop_route_wins_over_message_flag_and_trims() {
        let mut table = HashMap::new();
        table.insert(
            EVT_PULL_REQUEST_OPENED.to_string(),
            route(true, Some(" pr-triage ")),
        );
        assert_eq!(
            resolve_route(EVT_PULL_REQUEST_OPENED, &table),
            RouteAction::Sop {
                sop: "pr-triage".to_string(),
            }
        );
    }

    #[test]
    fn empty_sop_falls_back_to_message_flag() {
        let mut table = HashMap::new();
        table.insert(EVT_RELEASE_PUBLISHED.to_string(), route(true, Some("")));
        table.insert(
            EVT_WORKFLOW_RUN_FAILED.to_string(),
            route(false, Some("  ")),
        );
        assert_eq!(
            resolve_route(EVT_RELEASE_PUBLISHED, &table),
            RouteAction::Message
        );
        assert_eq!(
            resolve_route(EVT_WORKFLOW_RUN_FAILED, &table),
            RouteAction::Ignore
        );
    }

    #[test]
    fn validate_routes_flags_typos_and_empty_sop_names() {
        let mut table = HashMap::new();
        table.insert("pull_request.create".to_string(), route(true, None));
        table.insert(EVT_RELEASE_PUBLISHED.to_string(), route(false, Some(" ")));
        let problems = validate_routes(&table);
        assert_eq!(problems.len(), 2);
        assert!(problems.iter().any(|p| p.contains("pull_request.create")));
        assert!(problems.iter().any(|p| p.contains("empty `sop` name")));
        assert!(validate_routes(&HashMap::new()).is_empty());
    }

    #[test]
    fn transport_plan_defaults_to_conversational_endpoints() {
        let plan = TransportPlan::from_routes(&HashMap::new());
        assert!(plan.issues);
        assert!(plan.comments);
        assert!(!plan.review_comments);
        assert!(!plan.releases);
        assert!(!plan.workflow_runs);
        assert!(plan.any());
    }

    #[test]
    fn transport_plan_follows_routed_event_types() {
        let mut table = HashMap::new();
        table.insert(
            EVT_WORKFLOW_RUN_FAILED.to_string(),
            route(false, Some("ci-failure")),
        );
        table.insert(EVT_RELEASE_PUBLISHED.to_string(), route(true, None));
        let plan = TransportPlan::from_routes(&table);
        assert!(plan.workflow_runs);
        assert!(plan.releases);
        // Defaults still cover the conversational endpoints.
        assert!(plan.issues);
        assert!(plan.comments);
        assert!(!plan.review_comments);
    }

    #[test]
    fn transport_plan_empty_when_everything_opted_out() {
        let mut table = HashMap::new();
        for evt in [
            EVT_ISSUE_COMMENT_CREATED,
            EVT_ISSUES_OPENED,
            EVT_PULL_REQUEST_OPENED,
        ] {
            table.insert(evt.to_string(), route(false, None));
        }
        let plan = TransportPlan::from_routes(&table);
        assert!(!plan.any());
    }
}
