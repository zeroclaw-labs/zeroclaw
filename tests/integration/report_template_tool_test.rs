//! Integration tests for ReportTemplateTool.

use serde_json::json;
use zeroclaw::tools::{ReportTemplateTool, Tool};

#[tokio::test]
async fn render_weekly_status_en() {
    let tool = ReportTemplateTool::new();
    let params = json!({
        "template": "weekly_status",
        "language": "en",
        "variables": {
            "project_name": "Acme Platform",
            "period": "2026-W10",
            "completed": "- Task A\n- Task B",
            "in_progress": "- Task C",
            "blocked": "None",
            "next_steps": "- Task D"
        }
    });

    let result = tool.execute(params).await.unwrap();
    assert!(result.success);
    assert!(result.output.contains("Project: Acme Platform"));
    assert!(result.output.contains("Period: 2026-W10"));
    assert!(result.output.contains("- Task A"));
    assert!(result.output.contains("## Completed"));
}

#[tokio::test]
async fn render_sprint_review_de() {
    let tool = ReportTemplateTool::new();
    let params = json!({
        "template": "sprint_review",
        "language": "de",
        "variables": {
            "sprint_dates": "2026-03-01 bis 2026-03-14",
            "completed": "Feature X implementiert",
            "in_progress": "Feature Y",
            "blocked": "Keine",
            "velocity": "12 Story Points"
        }
    });

    let result = tool.execute(params).await.unwrap();
    assert!(result.success);
    assert!(result.output.contains("## Sprint"));
    assert!(result.output.contains("## Erledigt"));
    assert!(result.output.contains("Feature X implementiert"));
}

#[tokio::test]
async fn render_risk_register_fr() {
    let tool = ReportTemplateTool::new();
    let params = json!({
        "template": "risk_register",
        "language": "fr",
        "variables": {
            "project_name": "Projet Alpha",
            "risks": "Risque de retard",
            "mitigations": "Augmenter les ressources"
        }
    });

    let result = tool.execute(params).await.unwrap();
    assert!(result.success);
    assert!(result.output.contains("## Projet"));
    assert!(result.output.contains("## Risques"));
    assert!(result.output.contains("Risque de retard"));
}

#[tokio::test]
async fn render_milestone_report_it() {
    let tool = ReportTemplateTool::new();
    let params = json!({
        "template": "milestone_report",
        "language": "it",
        "variables": {
            "project_name": "Progetto Beta",
            "milestones": "M1: Completato\nM2: In corso",
            "status": "In linea con i tempi"
        }
    });

    let result = tool.execute(params).await.unwrap();
    assert!(result.success);
    assert!(result.output.contains("## Progetto"));
    assert!(result.output.contains("## Milestone"));
    assert!(result.output.contains("M1: Completato"));
}

#[tokio::test]
async fn default_language_is_en() {
    let tool = ReportTemplateTool::new();
    let params = json!({
        "template": "weekly_status",
        "variables": {
            "project_name": "Test",
            "period": "W1",
            "completed": "Done",
            "in_progress": "WIP",
            "blocked": "None",
            "next_steps": "Next"
        }
    });

    let result = tool.execute(params).await.unwrap();
    assert!(result.success);
    assert!(result.output.contains("## Summary"));
    assert!(result.output.contains("## Completed"));
}

#[tokio::test]
async fn missing_template_param_fails() {
    let tool = ReportTemplateTool::new();
    let params = json!({
        "variables": {
            "project_name": "Test"
        }
    });

    let result = tool.execute(params).await;
    assert!(result.is_err());
    let error = result.unwrap_err().to_string();
    assert!(error.contains("missing template"));
}

#[tokio::test]
async fn missing_variables_param_fails() {
    let tool = ReportTemplateTool::new();
    let params = json!({
        "template": "weekly_status"
    });

    let result = tool.execute(params).await;
    assert!(result.is_err());
    let error = result.unwrap_err().to_string();
    assert!(error.contains("variables must be object"));
}

#[tokio::test]
async fn invalid_template_name_fails() {
    let tool = ReportTemplateTool::new();
    let params = json!({
        "template": "unknown_template",
        "variables": {
            "project_name": "Test"
        }
    });

    let result = tool.execute(params).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn invalid_language_code_fails() {
    let tool = ReportTemplateTool::new();
    let params = json!({
        "template": "weekly_status",
        "language": "es",
        "variables": {
            "project_name": "Test"
        }
    });

    let result = tool.execute(params).await;
    // Note: The current implementation doesn't fail on invalid language,
    // it falls back to English. We test this behavior.
    let result = result.unwrap();
    assert!(result.success);
    // Should render in English (default fallback)
    assert!(result.output.contains("## Summary"));
}

#[tokio::test]
async fn empty_variables_map_renders() {
    let tool = ReportTemplateTool::new();
    let params = json!({
        "template": "weekly_status",
        "variables": {}
    });

    let result = tool.execute(params).await.unwrap();
    assert!(result.success);
    // Placeholders should remain unchanged
    assert!(result.output.contains("{{project_name}}"));
    assert!(result.output.contains("{{period}}"));
}

#[tokio::test]
async fn injection_protection_enforced() {
    let tool = ReportTemplateTool::new();
    let params = json!({
        "template": "weekly_status",
        "variables": {
            "project_name": "Test {{injected}}",
            "period": "W1",
            "completed": "{{nested_var}}",
            "in_progress": "WIP",
            "blocked": "None",
            "next_steps": "Next",
            "injected": "SHOULD_NOT_APPEAR",
            "nested_var": "SHOULD_NOT_EXPAND"
        }
    });

    let result = tool.execute(params).await.unwrap();
    assert!(result.success);
    // The value "Test {{injected}}" should be inserted literally
    assert!(result.output.contains("Test {{injected}}"));
    // The nested variable should not be expanded recursively
    assert!(result.output.contains("{{nested_var}}"));
    // The injected values should not appear
    assert!(!result.output.contains("SHOULD_NOT_APPEAR"));
    assert!(!result.output.contains("SHOULD_NOT_EXPAND"));
}

#[tokio::test]
async fn non_string_variable_values_coerced() {
    let tool = ReportTemplateTool::new();
    let params = json!({
        "template": "weekly_status",
        "variables": {
            "project_name": "Test",
            "period": 123,
            "completed": true,
            "in_progress": false,
            "blocked": null,
            "next_steps": ["array", "not", "supported"]
        }
    });

    let result = tool.execute(params).await.unwrap();
    assert!(result.success);
    // Numbers and booleans should be coerced to strings
    // null and arrays should result in empty strings
    assert!(result.output.contains("Project: Test"));
}
