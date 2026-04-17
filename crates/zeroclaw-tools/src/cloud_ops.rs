//! Cloud operations advisory tool for cloud transformation analysis.
//!
//! Provides read-only analysis capabilities: IaC review, migration assessment,
//! cost analysis, and Well-Architected Framework architecture review.
//! This tool does NOT create, modify, or delete cloud resources.

use crate::util_helpers::truncate_with_ellipsis;
use async_trait::async_trait;
use serde_json::json;
use zeroclaw_api::tool::{Tool, ToolResult};
use zeroclaw_config::schema::CloudOpsConfig;

/// Read-only cloud operations advisory tool.
///
/// Actions: `review_iac`, `assess_migration`, `cost_analysis`, `architecture_review`.
pub struct CloudOpsTool {
    config: CloudOpsConfig,
}

impl CloudOpsTool {
    pub fn new(config: CloudOpsConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl Tool for CloudOpsTool {
    fn name(&self) -> &str {
        "cloud_ops"
    }

    fn description(&self) -> &str {
        "Cloud transformation advisory tool. Analyzes IaC plans, assesses migration paths, \
         reviews costs, and checks architecture against Well-Architected Framework pillars. \
         Read-only: does not create or modify cloud resources."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["review_iac", "assess_migration", "cost_analysis", "architecture_review"],
                    "description": "The analysis action to perform."
                },
                "input": {
                    "type": "string",
                    "description": "For review_iac: IaC plan text or JSON content to analyze. For assess_migration: current architecture description text. For cost_analysis: billing data as CSV/JSON text. For architecture_review: architecture description text. Note: provide text content directly, not file paths."
                },
                "cloud": {
                    "type": "string",
                    "description": "Target cloud provider (aws, azure, gcp). Uses configured default if omitted."
                }
            },
            "required": ["action", "input"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let action = match args.get("action") {
            Some(v) => v
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("'action' must be a string, got: {}", v))?,
            None => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("'action' parameter is required".into()),
                });
            }
        };
        let input = match args.get("input") {
            Some(v) => v
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("'input' must be a string, got: {}", v))?,
            None => "",
        };
        let cloud = match args.get("cloud") {
            Some(v) => v
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("'cloud' must be a string, got: {}", v))?,
            None => &self.config.default_cloud,
        };

        if input.is_empty() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("'input' parameter is required and cannot be empty".into()),
            });
        }

        if !self.config.supported_clouds.contains(&cloud.to_string()) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Cloud provider '{}' is not in supported_clouds: {:?}",
                    cloud, self.config.supported_clouds
                )),
            });
        }

        match action {
            "review_iac" => self.review_iac(input, cloud).await,
            "assess_migration" => self.assess_migration(input, cloud).await,
            "cost_analysis" => self.cost_analysis(input, cloud).await,
            "architecture_review" => self.architecture_review(input, cloud).await,
            _ => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Unknown action '{}'. Valid: review_iac, assess_migration, cost_analysis, architecture_review",
                    action
                )),
            }),
        }
    }
}

#[allow(clippy::unused_async)]
impl CloudOpsTool {
    async fn review_iac(&self, input: &str, cloud: &str) -> anyhow::Result<ToolResult> {
        let mut findings = Vec::new();

        // Detect IaC type from content
        let iac_type = detect_iac_type(input);

        // Security findings
        for finding in scan_iac_security(input) {
            findings.push(finding);
        }

        // Best practice findings
        for finding in scan_iac_best_practices(input, cloud) {
            findings.push(finding);
        }

        // Cost implications
        for finding in scan_iac_cost(input, cloud, self.config.cost_threshold_monthly_usd) {
            findings.push(finding);
        }

        let output = json!({
            "iac_type": iac_type,
            "cloud": cloud,
            "findings_count": findings.len(),
            "findings": findings,
            "supported_iac_tools": self.config.iac_tools,
        });

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&output)?,
            error: None,
        })
    }

    async fn assess_migration(&self, input: &str, cloud: &str) -> anyhow::Result<ToolResult> {
        let recommendations = assess_migration_recommendations(input, cloud);

        let output = json!({
            "cloud": cloud,
            "source_description": truncate_with_ellipsis(input, 200),
            "recommendations": recommendations,
        });

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&output)?,
            error: None,
        })
    }

    async fn cost_analysis(&self, input: &str, cloud: &str) -> anyhow::Result<ToolResult> {
        let opportunities =
            analyze_cost_opportunities(input, self.config.cost_threshold_monthly_usd);

        let output = json!({
            "cloud": cloud,
            "threshold_usd": self.config.cost_threshold_monthly_usd,
            "opportunities_count": opportunities.len(),
            "opportunities": opportunities,
        });

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&output)?,
            error: None,
        })
    }

    async fn architecture_review(&self, input: &str, cloud: &str) -> anyhow::Result<ToolResult> {
        let frameworks = &self.config.well_architected_frameworks;
        let pillars = review_architecture_pillars(input, cloud, frameworks);

        let output = json!({
            "cloud": cloud,
            "frameworks": frameworks,
            "pillars": pillars,
        });

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&output)?,
            error: None,
        })
    }
}

// ── Analysis helpers ──────────────────────────────────────────────

fn detect_iac_type(input: &str) -> &'static str {
    let lower = input.to_lowercase();
    if lower.contains("resource \"") || lower.contains("terraform") || lower.contains(".tf") {
        "terraform"
    } else if lower.contains("awstemplatebody")
        || lower.contains("cloudformation")
        || lower.contains("aws::")
    {
        "cloudformation"
    } else if lower.contains("pulumi") {
        "pulumi"
    } else {
        "unknown"
    }
}

/// Scan IaC content for common security issues.
fn scan_iac_security(input: &str) -> Vec<serde_json::Value> {
    let lower = input.to_lowercase();
    let mut findings = Vec::new();

    let security_patterns: &[(&str, &str, &str)] = &[
        (
            "0.0.0.0/0",
            "high",
            "Unrestricted ingress (0.0.0.0/0) detected. Restrict CIDR ranges to known networks.",
        ),
        (
            "::/0",
            "high",
            "Unrestricted IPv6 ingress (::/0) detected. Restrict CIDR ranges.",
        ),
        (
            "public_access",
            "medium",
            "Public access setting detected. Verify this is intentional and necessary.",
        ),
        (
            "publicly_accessible",
            "medium",
            "Resource marked as publicly accessible. Ensure this is required.",
        ),
        (
            "encrypted = false",
            "high",
            "Encryption explicitly disabled. Enable encryption at rest.",
        ),
        (
            "\"*\"",
            "medium",
            "Wildcard permission detected. Follow least-privilege principle.",
        ),
        (
            "password",
            "medium",
            "Hardcoded password reference detected. Use secrets manager instead.",
        ),
        (
            "access_key",
            "high",
            "Access key reference in IaC. Use IAM roles or secrets manager.",
        ),
        (
            "secret_key",
            "high",
            "Secret key reference in IaC. Use IAM roles or secrets manager.",
        ),
    ];

    for (pattern, severity, message) in security_patterns {
        if lower.contains(pattern) {
            findings.push(json!({
                "category": "security",
                "severity": severity,
                "message": message,
            }));
        }
    }

    findings
}

/// Scan for IaC best practice violations.
fn scan_iac_best_practices(input: &str, cloud: &str) -> Vec<serde_json::Value> {
    let lower = input.to_lowercase();
    let mut findings = Vec::new();

    // Tagging
    if !lower.contains("tags") && !lower.contains("tag") {
        findings.push(json!({
            "category": "best_practice",
            "severity": "low",
            "message": "No resource tags detected. Add tags for cost allocation and resource management.",
        }));
    }

    // Versioning
    if lower.contains("s3") && !lower.contains("versioning") {
        findings.push(json!({
            "category": "best_practice",
            "severity": "medium",
            "message": "S3 bucket without versioning detected. Enable versioning for data protection.",
        }));
    }

    // Logging
    if !lower.contains("logging") && !lower.contains("log_group") && !lower.contains("access_logs")
    {
        findings.push(json!({
            "category": "best_practice",
            "severity": "low",
            "message": format!("No logging configuration detected for {}. Enable access logging.", cloud),
        }));
    }

    // Backup
    if lower.contains("rds") && !lower.contains("backup_retention") {
        findings.push(json!({
            "category": "best_practice",
            "severity": "medium",
            "message": "RDS instance without backup retention configuration. Set backup_retention_period.",
        }));
    }

    findings
}

/// Scan for cost-related observations in IaC.
///
/// Only emits findings for resources whose estimated monthly cost exceeds
/// `threshold`.  AWS-specific patterns (NAT Gateway, Elastic IP, ALB) are
/// gated behind `cloud == "aws"`.
fn scan_iac_cost(input: &str, cloud: &str, threshold: f64) -> Vec<serde_json::Value> {
    let lower = input.to_lowercase();
    let mut findings = Vec::new();

    // (pattern, message, estimated_monthly_usd, aws_only)
    let expensive_patterns: &[(&str, &str, f64, bool)] = &[
        (
            "instance_type",
            "Review instance sizing. Consider right-sizing or spot/preemptible instances.",
            50.0,
            false,
        ),
        (
            "nat_gateway",
            "NAT Gateway detected. These incur hourly + data transfer charges. Consider VPC endpoints for AWS services.",
            45.0,
            true,
        ),
        (
            "elastic_ip",
            "Elastic IP detected. Unused EIPs incur charges.",
            5.0,
            true,
        ),
        (
            "load_balancer",
            "Load balancer detected. Verify it is needed; consider ALB over NLB/CLB for cost.",
            25.0,
            true,
        ),
    ];

    for (pattern, message, estimated_cost, aws_only) in expensive_patterns {
        if *aws_only && cloud != "aws" {
            continue;
        }
        if *estimated_cost < threshold {
            continue;
        }
        if lower.contains(pattern) {
            findings.push(json!({
                "category": "cost",
                "severity": "info",
                "message": message,
                "estimated_monthly_usd": estimated_cost,
            }));
        }
    }

    findings
}

/// Generate migration recommendations based on architecture description.
fn assess_migration_recommendations(input: &str, cloud: &str) -> Vec<serde_json::Value> {
    let lower = input.to_lowercase();
    let mut recs = Vec::new();

    let migration_patterns: &[(&str, &str, &str, &str)] = &[
        (
            "monolith",
            "Decompose into microservices or modular containers.",
            "high",
            "Consider containerizing with ECS/EKS (AWS), AKS (Azure), or GKE (GCP).",
        ),
        (
            "vm",
            "Migrate VMs to containers or serverless where feasible.",
            "medium",
            "Evaluate lift-and-shift to managed container services.",
        ),
        (
            "on-premises",
            "Assess workloads for cloud readiness using 6 Rs framework (rehost, replatform, refactor, repurchase, retire, retain).",
            "high",
            "Start with rehost for quick migration, then optimize.",
        ),
        (
            "database",
            "Evaluate managed database services for reduced operational overhead.",
            "medium",
            &format!(
                "Consider managed options: RDS/Aurora (AWS), Azure SQL (Azure), Cloud SQL (GCP) for {}.",
                cloud
            ),
        ),
        (
            "batch",
            "Consider serverless compute for batch workloads.",
            "low",
            "Evaluate Lambda (AWS), Azure Functions, or Cloud Functions for event-driven batch.",
        ),
        (
            "queue",
            "Evaluate managed message queue services.",
            "low",
            "Consider SQS/SNS (AWS), Service Bus (Azure), or Pub/Sub (GCP).",
        ),
        (
            "storage",
            "Evaluate tiered object storage for cost optimization.",
            "medium",
            "Use lifecycle policies for infrequent access data.",
        ),
        (
            "legacy",
            "Assess modernization path: replatform or refactor.",
            "high",
            "Legacy systems carry tech debt; prioritize incremental modernization.",
        ),
    ];

    for (keyword, recommendation, effort, detail) in migration_patterns {
        if lower.contains(keyword) {
            recs.push(json!({
                "trigger": keyword,
                "recommendation": recommendation,
                "effort_estimate": effort,
                "detail": detail,
                "target_cloud": cloud,
            }));
        }
    }

    if recs.is_empty() {
        recs.push(json!({
            "trigger": "general",
            "recommendation": "Provide more detail about current architecture components for targeted recommendations.",
            "effort_estimate": "unknown",
            "detail": "Include details about compute, storage, networking, and data layers.",
            "target_cloud": cloud,
        }));
    }

    recs
}

/// Analyze billing/cost data for optimization opportunities.
fn analyze_cost_opportunities(input: &str, threshold: f64) -> Vec<serde_json::Value> {
    let lower = input.to_lowercase();
    let mut opportunities = Vec::new();

    // General cost patterns
    let cost_patterns: &[(&str, &str, &str)] = &[
        (
            "reserved",
            "Review reserved instance utilization. Unused reservations waste budget.",
            "high",
        ),
        (
            "on-demand",
            "On-demand instances detected. Evaluate savings plans or reserved instances for stable workloads.",
            "high",
        ),
        (
            "data transfer",
            "Data transfer costs detected. Use VPC endpoints, CDN, or regional placement to reduce.",
            "medium",
        ),
        (
            "storage",
            "Storage costs detected. Implement lifecycle policies and tiered storage.",
            "medium",
        ),
        (
            "idle",
            "Idle resources detected. Identify and terminate unused resources.",
            "high",
        ),
        (
            "unattached",
            "Unattached resources (volumes, IPs) detected. Clean up to reduce waste.",
            "medium",
        ),
        (
            "snapshot",
            "Snapshot costs detected. Review retention policies and delete stale snapshots.",
            "low",
        ),
    ];

    for (pattern, suggestion, priority) in cost_patterns {
        if lower.contains(pattern) {
            opportunities.push(json!({
                "pattern": pattern,
                "suggestion": suggestion,
                "priority": priority,
                "threshold_usd": threshold,
            }));
        }
    }

    if opportunities.is_empty() {
        opportunities.push(json!({
            "pattern": "general",
            "suggestion": "Provide billing CSV/JSON data with service and cost columns for detailed analysis.",
            "priority": "info",
            "threshold_usd": threshold,
        }));
    }

    opportunities
}

/// Review architecture against Well-Architected Framework pillars.
fn review_architecture_pillars(
    input: &str,
    cloud: &str,
    _frameworks: &[String],
) -> Vec<serde_json::Value> {
    let lower = input.to_lowercase();

    let pillars = vec![
        ("security", review_pillar_security(&lower, cloud)),
        ("reliability", review_pillar_reliability(&lower, cloud)),
        ("performance", review_pillar_performance(&lower, cloud)),
        ("cost_optimization", review_pillar_cost(&lower, cloud)),
        (
            "operational_excellence",
            review_pillar_operations(&lower, cloud),
        ),
    ];

    pillars
        .into_iter()
        .map(|(name, findings)| {
            json!({
                "pillar": name,
                "findings_count": findings.len(),
                "findings": findings,
            })
        })
        .collect()
}

fn review_pillar_security(input: &str, _cloud: &str) -> Vec<String> {
    let mut findings = Vec::new();
    if !input.contains("iam") && !input.contains("identity") {
        findings.push(
            "No IAM/identity layer described. Define identity and access management strategy."
                .into(),
        );
    }
    if !input.contains("encrypt") {
        findings
            .push("No encryption mentioned. Implement encryption at rest and in transit.".into());
    }
    if !input.contains("firewall") && !input.contains("waf") && !input.contains("security group") {
        findings.push(
            "No network security controls described. Add WAF, security groups, or firewall rules."
                .into(),
        );
    }
    if !input.contains("audit") && !input.contains("logging") {
        findings.push(
            "No audit logging described. Enable CloudTrail/Azure Monitor/Cloud Audit Logs.".into(),
        );
    }
    findings
}

fn review_pillar_reliability(input: &str, _cloud: &str) -> Vec<String> {
    let mut findings = Vec::new();
    if !input.contains("multi-az") && !input.contains("multi-region") && !input.contains("redundan")
    {
        findings
            .push("No redundancy described. Consider multi-AZ or multi-region deployment.".into());
    }
    if !input.contains("backup") {
        findings.push("No backup strategy described. Define RPO/RTO and backup schedules.".into());
    }
    if !input.contains("auto-scal") && !input.contains("autoscal") {
        findings.push(
            "No auto-scaling described. Implement scaling policies for variable load.".into(),
        );
    }
    if !input.contains("health check") && !input.contains("monitor") {
        findings.push("No health monitoring described. Add health checks and alerting.".into());
    }
    findings
}

fn review_pillar_performance(input: &str, _cloud: &str) -> Vec<String> {
    let mut findings = Vec::new();
    if !input.contains("cache") && !input.contains("cdn") {
        findings
            .push("No caching layer described. Consider CDN and application-level caching.".into());
    }
    if !input.contains("load balanc") {
        findings
            .push("No load balancing described. Add load balancer for distributed traffic.".into());
    }
    if !input.contains("metric") && !input.contains("benchmark") {
        findings.push(
            "No performance metrics described. Define SLIs/SLOs and baseline benchmarks.".into(),
        );
    }
    findings
}

fn review_pillar_cost(input: &str, _cloud: &str) -> Vec<String> {
    let mut findings = Vec::new();
    if !input.contains("budget") && !input.contains("cost") {
        findings
            .push("No cost controls described. Set budget alerts and cost allocation tags.".into());
    }
    if !input.contains("reserved") && !input.contains("savings plan") && !input.contains("spot") {
        findings.push("No cost optimization strategy described. Evaluate RIs, savings plans, or spot instances.".into());
    }
    if !input.contains("rightsiz") && !input.contains("right-siz") {
        findings.push(
            "No right-sizing mentioned. Regularly review instance utilization and downsize.".into(),
        );
    }
    findings
}

fn review_pillar_operations(input: &str, _cloud: &str) -> Vec<String> {
    let mut findings = Vec::new();
    if !input.contains("iac")
        && !input.contains("terraform")
        && !input.contains("infrastructure as code")
    {
        findings.push(
            "No IaC mentioned. Manage all infrastructure as code for reproducibility.".into(),
        );
    }
    if !input.contains("ci") && !input.contains("pipeline") && !input.contains("deploy") {
        findings.push("No CI/CD described. Automate build, test, and deployment pipelines.".into());
    }
    if !input.contains("runbook") && !input.contains("incident") {
        findings.push(
            "No incident response described. Create runbooks and incident procedures.".into(),
        );
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> CloudOpsConfig {
        CloudOpsConfig::default()
    }

    #[tokio::test]
    async fn review_iac_detects_security_findings() {
        let tool = CloudOpsTool::new(test_config());
        let result = tool
            .execute(json!({
                "action": "review_iac",
                "input": "resource \"aws_security_group\" \"open\" { ingress { cidr_blocks = [\"0.0.0.0/0\"] } }"
            }))
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.output.contains("Unrestricted ingress"));
        assert!(result.output.contains("high"));
    }

    #[tokio::test]
    async fn review_iac_detects_terraform_type() {
        let tool = CloudOpsTool::new(test_config());
        let result = tool
            .execute(json!({
                "action": "review_iac",
                "input": "resource \"aws_instance\" \"test\" { instance_type = \"t3.micro\" tags = { Name = \"test\" } }"
            }))
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.output.contains("\"iac_type\": \"terraform\""));
    }

    #[tokio::test]
    async fn review_iac_detects_encrypted_false() {
        let tool = CloudOpsTool::new(test_config());
        let result = tool
            .execute(json!({
                "action": "review_iac",
                "input": "resource \"aws_ebs_volume\" \"vol\" { encrypted = false tags = {} }"
            }))
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.output.contains("Encryption explicitly disabled"));
    }

    #[tokio::test]
    async fn cost_analysis_detects_on_demand() {
        let tool = CloudOpsTool::new(test_config());
        let result = tool
            .execute(json!({
                "action": "cost_analysis",
                "input": "service,cost\nEC2 On-Demand,5000\nS3 Storage,200"
            }))
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.output.contains("on-demand"));
        assert!(result.output.contains("storage"));
    }

    #[tokio::test]
    async fn architecture_review_returns_all_pillars() {
        let tool = CloudOpsTool::new(test_config());
        let result = tool
            .execute(json!({
                "action": "architecture_review",
                "input": "Web app with EC2, RDS, S3. No caching layer."
            }))
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.output.contains("security"));
        assert!(result.output.contains("reliability"));
        assert!(result.output.contains("performance"));
        assert!(result.output.contains("cost_optimization"));
        assert!(result.output.contains("operational_excellence"));
    }

    #[tokio::test]
    async fn assess_migration_detects_monolith() {
        let tool = CloudOpsTool::new(test_config());
        let result = tool
            .execute(json!({
                "action": "assess_migration",
                "input": "Legacy monolith application running on VMs with on-premises database."
            }))
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.output.contains("monolith"));
        assert!(result.output.contains("microservices"));
    }

    #[tokio::test]
    async fn empty_input_returns_error() {
        let tool = CloudOpsTool::new(test_config());
        let result = tool
            .execute(json!({
                "action": "review_iac",
                "input": ""
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.is_some());
    }

    #[tokio::test]
    async fn unsupported_cloud_returns_error() {
        let tool = CloudOpsTool::new(test_config());
        let result = tool
            .execute(json!({
                "action": "review_iac",
                "input": "some content",
                "cloud": "alibaba"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.unwrap().contains("not in supported_clouds"));
    }

    #[tokio::test]
    async fn unknown_action_returns_error() {
        let tool = CloudOpsTool::new(test_config());
        let result = tool
            .execute(json!({
                "action": "deploy_everything",
                "input": "some content"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.unwrap().contains("Unknown action"));
    }

    #[test]
    fn detect_iac_type_identifies_cloudformation() {
        assert_eq!(detect_iac_type("AWS::EC2::Instance"), "cloudformation");
    }

    #[test]
    fn detect_iac_type_identifies_pulumi() {
        assert_eq!(detect_iac_type("import pulumi"), "pulumi");
    }

    #[test]
    fn scan_iac_security_finds_wildcard_permission() {
        let findings = scan_iac_security("Action: \"*\" Effect: Allow");
        assert!(!findings.is_empty());
        let msg = findings[0]["message"].as_str().unwrap();
        assert!(msg.contains("Wildcard permission"));
    }

    #[test]
    fn scan_iac_cost_gates_aws_patterns_for_non_aws() {
        // NAT Gateway / Elastic IP / Load Balancer are AWS-only; should not appear for azure
        let findings = scan_iac_cost(
            "nat_gateway elastic_ip load_balancer instance_type",
            "azure",
            0.0, // threshold 0 so all cost-eligible items pass
        );
        for f in &findings {
            let msg = f["message"].as_str().unwrap();
            assert!(
                !msg.contains("NAT Gateway") && !msg.contains("Elastic IP") && !msg.contains("ALB"),
                "AWS-specific finding leaked for azure: {}",
                msg
            );
        }
        // instance_type is cloud-agnostic and should still appear
        assert!(
            findings
                .iter()
                .any(|f| f["message"].as_str().unwrap().contains("instance sizing"))
        );
    }

    #[test]
    fn scan_iac_cost_respects_threshold() {
        // With a high threshold, low-cost patterns should be filtered out
        let findings = scan_iac_cost(
            "nat_gateway elastic_ip instance_type",
            "aws",
            200.0, // above all estimated costs
        );
        assert!(
            findings.is_empty(),
            "expected no findings above threshold 200, got {:?}",
            findings
        );
    }

    #[tokio::test]
    async fn non_string_action_returns_error() {
        let tool = CloudOpsTool::new(test_config());
        let result = tool
            .execute(json!({
                "action": 42,
                "input": "some content"
            }))
            .await;

        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("'action' must be a string"));
    }

    #[tokio::test]
    async fn non_string_input_returns_error() {
        let tool = CloudOpsTool::new(test_config());
        let result = tool
            .execute(json!({
                "action": "review_iac",
                "input": 123
            }))
            .await;

        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("'input' must be a string"));
    }

    #[tokio::test]
    async fn non_string_cloud_returns_error() {
        let tool = CloudOpsTool::new(test_config());
        let result = tool
            .execute(json!({
                "action": "review_iac",
                "input": "some content",
                "cloud": true
            }))
            .await;

        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("'cloud' must be a string"));
    }
}
