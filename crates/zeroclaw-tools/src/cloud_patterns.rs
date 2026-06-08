//! Cloud pattern library for recommending cloud-native architectural patterns.
//!
//! Provides a built-in set of cloud migration and modernization patterns,
//! with pattern matching against workload descriptions.

use crate::util_helpers::truncate_with_ellipsis;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;
use zeroclaw_api::tool::{Tool, ToolResult};

/// A cloud architecture pattern with metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudPattern {
    pub name: String,
    pub description: String,
    pub cloud_providers: Vec<String>,
    pub use_case: String,
    pub example_iac: String,
    /// Keywords for matching against workload descriptions.
    keywords: Vec<String>,
}

/// Tool that suggests cloud patterns given a workload description.
pub struct CloudPatternsTool {
    patterns: Vec<CloudPattern>,
}

impl Default for CloudPatternsTool {
    fn default() -> Self {
        Self::new()
    }
}

impl CloudPatternsTool {
    pub fn new() -> Self {
        Self {
            patterns: built_in_patterns(),
        }
    }
}

#[async_trait]
impl Tool for CloudPatternsTool {
    fn name(&self) -> &str {
        "cloud_patterns"
    }

    fn description(&self) -> &str {
        "Cloud pattern library. Given a workload description, suggests applicable cloud-native \
         architectural patterns (containerization, serverless, database modernization, etc.)."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["match", "list"],
                    "description": "Action: 'match' to find patterns for a workload, 'list' to show all patterns."
                },
                "workload": {
                    "type": "string",
                    "description": "Description of the workload to match patterns against (required for 'match')."
                },
                "cloud": {
                    "type": "string",
                    "description": "Filter patterns by cloud model_provider (aws, azure, gcp). Optional."
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let workload = args
            .get("workload")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let cloud_filter = args.get("cloud").and_then(|v| v.as_str());

        match action {
            "list" => {
                let filtered = self.filter_by_cloud(cloud_filter);
                let summaries: Vec<serde_json::Value> = filtered
                    .iter()
                    .map(|p| {
                        json!({
                            "name": p.name,
                            "description": p.description,
                            "cloud_providers": p.cloud_providers,
                            "use_case": p.use_case,
                        })
                    })
                    .collect();

                let output = json!({
                    "patterns_count": summaries.len(),
                    "patterns": summaries,
                });

                Ok(ToolResult {
                    success: true,
                    output: serde_json::to_string_pretty(&output)?,
                    error: None,
                })
            }
            "match" => {
                if workload.trim().is_empty() {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some("'workload' parameter is required for 'match' action".into()),
                    });
                }

                let matched = self.match_patterns(workload, cloud_filter);

                let output = json!({
                    "workload_summary": truncate_with_ellipsis(workload, 200),
                    "matched_count": matched.len(),
                    "matched_patterns": matched,
                });

                Ok(ToolResult {
                    success: true,
                    output: serde_json::to_string_pretty(&output)?,
                    error: None,
                })
            }
            _ => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Unknown action '{}'. Valid: match, list", action)),
            }),
        }
    }
}

impl CloudPatternsTool {
    fn filter_by_cloud(&self, cloud: Option<&str>) -> Vec<&CloudPattern> {
        match cloud {
            Some(c) => self
                .patterns
                .iter()
                .filter(|p| p.cloud_providers.iter().any(|cp| cp == c))
                .collect(),
            None => self.patterns.iter().collect(),
        }
    }

    fn match_patterns(&self, workload: &str, cloud: Option<&str>) -> Vec<serde_json::Value> {
        let lower = workload.to_lowercase();
        let candidates = self.filter_by_cloud(cloud);

        let mut scored: Vec<(&CloudPattern, usize)> = candidates
            .into_iter()
            .filter_map(|p| {
                let score: usize = p
                    .keywords
                    .iter()
                    .filter(|kw| lower.contains(kw.as_str()))
                    .count();
                if score > 0 { Some((p, score)) } else { None }
            })
            .collect();

        scored.sort_by_key(|entry| std::cmp::Reverse(entry.1));

        // Built-in IaC examples are AWS Terraform only; include them only when
        // the cloud filter is unset or explicitly "aws".
        let include_example = cloud.is_none() || cloud == Some("aws");

        scored
            .into_iter()
            .map(|(p, score)| {
                let mut entry = json!({
                    "name": p.name,
                    "description": p.description,
                    "cloud_providers": p.cloud_providers,
                    "use_case": p.use_case,
                    "relevance_score": score,
                });
                if include_example {
                    entry["example_iac"] = json!(p.example_iac);
                }
                entry
            })
            .collect()
    }
}

fn built_in_patterns() -> Vec<CloudPattern> {
    vec![
        CloudPattern {
            name: "containerization".into(),
            description: "Package applications into containers for portability and consistent deployment.".into(),
            cloud_providers: vec!["aws".into(), "azure".into(), "gcp".into()],
            use_case: "Modernizing monolithic applications, improving deployment consistency, enabling microservices.".into(),
            example_iac: r#"# Terraform ECS Fargate example
resource "aws_ecs_cluster" "main" {
  name = "app-cluster"
}
resource "aws_ecs_service" "app" {
  cluster         = aws_ecs_cluster.main.id
  task_definition = aws_ecs_task_definition.app.arn
  launch_type     = "FARGATE"
  desired_count   = 2
}"#.into(),
            keywords: vec!["container".into(), "docker".into(), "monolith".into(), "microservice".into(), "ecs".into(), "aks".into(), "gke".into(), "kubernetes".into(), "k8s".into()],
        },
        CloudPattern {
            name: "serverless_migration".into(),
            description: "Migrate event-driven or periodic workloads to serverless compute.".into(),
            cloud_providers: vec!["aws".into(), "azure".into(), "gcp".into()],
            use_case: "Batch jobs, API backends, event processing, cron tasks with variable load.".into(),
            example_iac: r#"# Terraform Lambda example
resource "aws_lambda_function" "handler" {
  function_name = "event-handler"
  runtime       = "python3.12"
  handler       = "main.handler"
  filename      = "handler.zip"
  memory_size   = 256
  timeout       = 30
}"#.into(),
            keywords: vec!["serverless".into(), "lambda".into(), "function".into(), "event".into(), "batch".into(), "cron".into(), "api".into(), "webhook".into()],
        },
        CloudPattern {
            name: "database_modernization".into(),
            description: "Migrate self-managed databases to cloud-managed services for reduced ops overhead.".into(),
            cloud_providers: vec!["aws".into(), "azure".into(), "gcp".into()],
            use_case: "Self-managed MySQL/PostgreSQL/SQL Server migration, NoSQL adoption, read replica scaling.".into(),
            example_iac: r#"# Terraform RDS example
resource "aws_db_instance" "main" {
  engine               = "postgres"
  engine_version       = "15"
  instance_class       = "db.t3.medium"
  allocated_storage    = 100
  multi_az             = true
  backup_retention_period = 7
  storage_encrypted    = true
}"#.into(),
            keywords: vec!["database".into(), "mysql".into(), "postgres".into(), "sql".into(), "rds".into(), "nosql".into(), "dynamo".into(), "mongodb".into(), "migration".into()],
        },
        CloudPattern {
            name: "api_gateway".into(),
            description: "Centralize API management with rate limiting, auth, and routing.".into(),
            cloud_providers: vec!["aws".into(), "azure".into(), "gcp".into()],
            use_case: "Public API exposure, microservice routing, API versioning, throttling.".into(),
            example_iac: r#"# Terraform API Gateway example
resource "aws_apigatewayv2_api" "main" {
  name          = "app-api"
  protocol_type = "HTTP"
}
resource "aws_apigatewayv2_stage" "prod" {
  api_id      = aws_apigatewayv2_api.main.id
  name        = "prod"
  auto_deploy = true
}"#.into(),
            keywords: vec!["api".into(), "gateway".into(), "rest".into(), "graphql".into(), "routing".into(), "rate limit".into(), "throttl".into()],
        },
        CloudPattern {
            name: "service_mesh".into(),
            description: "Implement service mesh for observability, traffic management, and security between microservices.".into(),
            cloud_providers: vec!["aws".into(), "azure".into(), "gcp".into()],
            use_case: "Microservice communication, mTLS, traffic splitting, canary deployments.".into(),
            example_iac: r#"# AWS App Mesh example
resource "aws_appmesh_mesh" "main" {
  name = "app-mesh"
}
resource "aws_appmesh_virtual_service" "app" {
  name      = "app.local"
  mesh_name = aws_appmesh_mesh.main.name
}"#.into(),
            keywords: vec!["mesh".into(), "istio".into(), "envoy".into(), "sidecar".into(), "mtls".into(), "canary".into(), "traffic".into(), "microservice".into()],
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_patterns_are_populated() {
        let patterns = built_in_patterns();
        assert_eq!(patterns.len(), 5);
        let names: Vec<&str> = patterns.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"containerization"));
        assert!(names.contains(&"serverless_migration"));
        assert!(names.contains(&"database_modernization"));
        assert!(names.contains(&"api_gateway"));
        assert!(names.contains(&"service_mesh"));
    }

    #[tokio::test]
    async fn match_returns_containerization_for_monolith() {
        let tool = CloudPatternsTool::new();
        let result = tool
            .execute(json!({
                "action": "match",
                "workload": "We have a monolith Java application running on VMs that we want to containerize."
            }))
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.output.contains("containerization"));
    }

    #[tokio::test]
    async fn match_returns_serverless_for_batch_workload() {
        let tool = CloudPatternsTool::new();
        let result = tool
            .execute(json!({
                "action": "match",
                "workload": "Batch processing cron jobs that handle event data"
            }))
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.output.contains("serverless_migration"));
    }

    #[tokio::test]
    async fn match_filters_by_cloud_provider() {
        let tool = CloudPatternsTool::new();
        let result = tool
            .execute(json!({
                "action": "match",
                "workload": "Container deployment with Kubernetes",
                "cloud": "aws"
            }))
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.output.contains("containerization"));
    }

    #[tokio::test]
    async fn list_returns_all_patterns() {
        let tool = CloudPatternsTool::new();
        let result = tool
            .execute(json!({
                "action": "list"
            }))
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.output.contains("\"patterns_count\": 5"));
    }

    #[tokio::test]
    async fn match_with_empty_workload_returns_error() {
        let tool = CloudPatternsTool::new();
        let result = tool
            .execute(json!({
                "action": "match",
                "workload": ""
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.is_some());
    }

    #[tokio::test]
    async fn match_database_workload_finds_db_modernization() {
        let tool = CloudPatternsTool::new();
        let result = tool
            .execute(json!({
                "action": "match",
                "workload": "Self-hosted PostgreSQL database needs migration to managed service"
            }))
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.output.contains("database_modernization"));
    }

    #[test]
    fn pattern_matching_scores_correctly() {
        let tool = CloudPatternsTool::new();
        let matches =
            tool.match_patterns("microservice container docker kubernetes deployment", None);
        // containerization should rank highest (most keyword matches)
        assert!(!matches.is_empty());
        assert_eq!(matches[0]["name"], "containerization");
    }

    #[tokio::test]
    async fn unknown_action_returns_error() {
        let tool = CloudPatternsTool::new();
        let result = tool
            .execute(json!({
                "action": "deploy"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.unwrap().contains("Unknown action"));
    }
}
