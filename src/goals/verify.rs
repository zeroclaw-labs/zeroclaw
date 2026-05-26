use crate::goals::{Goal, VerificationMethod};
use anyhow::Result;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct VerificationOutcome {
    pub passed: bool,
    pub evidence: String,
}

/// Verify that a goal has been satisfied. Returns whether the verification passed
/// and a human-readable evidence string explaining what was checked.
///
/// For [`VerificationMethod::AgentSelfReport`], the agent's response is trusted
/// (backward-compatible behavior). Other methods perform concrete checks.
pub async fn verify_goal(goal: &Goal, agent_response: &str) -> Result<VerificationOutcome> {
    match &goal.verification_method {
        VerificationMethod::AgentSelfReport => Ok(VerificationOutcome {
            passed: true,
            evidence: format!("agent self-report: {}", truncate(agent_response, 500)),
        }),
        VerificationMethod::Manual => Ok(VerificationOutcome {
            passed: false,
            evidence: "manual verification required; goal reverted to approved".to_string(),
        }),
        VerificationMethod::HealthOk { component } => {
            let snapshot = crate::health::snapshot_json();
            let status = snapshot["components"][component]["status"]
                .as_str()
                .unwrap_or("unknown");
            let passed = status == "ok";
            Ok(VerificationOutcome {
                passed,
                evidence: format!("health.{component}.status = {status}"),
            })
        }
        VerificationMethod::Command {
            cmd,
            expect_exit_zero,
        } => verify_command(cmd, *expect_exit_zero).await,
    }
}

async fn verify_command(cmd: &str, expect_exit_zero: bool) -> Result<VerificationOutcome> {
    let output = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .output()
        .await?;
    let exit_zero = output.status.success();
    let passed = exit_zero == expect_exit_zero;
    let stdout_preview = truncate(&String::from_utf8_lossy(&output.stdout), 200);
    let stderr_preview = truncate(&String::from_utf8_lossy(&output.stderr), 200);
    Ok(VerificationOutcome {
        passed,
        evidence: format!(
            "exit={} (expect_zero={expect_exit_zero}); stdout={stdout_preview}; stderr={stderr_preview}",
            output.status.code().map_or("signal".to_string(), |c| c.to_string()),
        ),
    })
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::goals::{GoalSource, GoalStatus};
    use chrono::Utc;

    fn make_goal(method: VerificationMethod) -> Goal {
        Goal {
            id: "test".to_string(),
            title: "test goal".to_string(),
            description: String::new(),
            source: GoalSource::default(),
            status: GoalStatus::InProgress,
            priority: 5,
            proposed_at: Utc::now(),
            approved_at: None,
            completed_at: None,
            evidence: None,
            success_criteria: Vec::new(),
            verification_method: method,
        }
    }

    #[tokio::test]
    async fn self_report_always_passes() {
        let g = make_goal(VerificationMethod::AgentSelfReport);
        let out = verify_goal(&g, "did the thing").await.unwrap();
        assert!(out.passed);
    }

    #[tokio::test]
    async fn manual_never_passes() {
        let g = make_goal(VerificationMethod::Manual);
        let out = verify_goal(&g, "agent says done").await.unwrap();
        assert!(!out.passed);
    }

    #[tokio::test]
    async fn command_exit_zero_passes() {
        let g = make_goal(VerificationMethod::Command {
            cmd: "true".to_string(),
            expect_exit_zero: true,
        });
        let out = verify_goal(&g, "").await.unwrap();
        assert!(out.passed, "evidence: {}", out.evidence);
    }

    #[tokio::test]
    async fn command_nonzero_when_expecting_zero_fails() {
        let g = make_goal(VerificationMethod::Command {
            cmd: "false".to_string(),
            expect_exit_zero: true,
        });
        let out = verify_goal(&g, "").await.unwrap();
        assert!(!out.passed);
    }

    #[tokio::test]
    async fn health_ok_reflects_snapshot() {
        crate::health::mark_component_ok("verify-test-ok");
        let g = make_goal(VerificationMethod::HealthOk {
            component: "verify-test-ok".to_string(),
        });
        let out = verify_goal(&g, "").await.unwrap();
        assert!(out.passed);

        crate::health::mark_component_error("verify-test-err", "boom");
        let g_err = make_goal(VerificationMethod::HealthOk {
            component: "verify-test-err".to_string(),
        });
        let out_err = verify_goal(&g_err, "").await.unwrap();
        assert!(!out_err.passed);
    }
}
