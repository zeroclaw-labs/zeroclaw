//! Concrete [`CodeReviewer`] implementations.
//!
//! Each reviewer wraps a specific model API and transforms the review
//! context into a model-specific prompt, then parses the response into
//! a structured [`ReviewReport`].

use async_trait::async_trait;
use serde::Deserialize;
use std::time::Instant;

use super::traits::{
    CodeReviewer, ReviewContext, ReviewFinding, ReviewReport, ReviewVerdict, Severity,
};

// ── Gemini reviewer (architecture gatekeeper) ────────────────────

/// Code reviewer powered by Google Gemini API.
///
/// Acts as the architecture quality gatekeeper, verifying that code
/// changes align with the project's design principles and roadmap.
pub struct GeminiReviewer {
    /// Reviewer identifier.
    id: String,
    /// Gemini API key.
    api_key: String,
    /// Model to use (e.g. "gemini-2.5-flash", "gemini-2.5-pro").
    model: String,
    /// API endpoint.
    endpoint: String,
    /// HTTP client.
    client: reqwest::Client,
}

impl GeminiReviewer {
    /// Create a new Gemini reviewer.
    pub fn new(api_key: String, model: String) -> Self {
        Self {
            id: format!(
                "gemini-{}",
                model.split('-').next_back().unwrap_or("reviewer")
            ),
            endpoint: format!(
                "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent",
                model
            ),
            api_key,
            model,
            client: reqwest::Client::new(),
        }
    }

    /// Build the review prompt for Gemini.
    fn build_prompt(ctx: &ReviewContext) -> String {
        let prior_summary = if ctx.prior_reviews.is_empty() {
            String::new()
        } else {
            let mut s = String::from("\n\n## Prior Review Reports\n");
            for r in &ctx.prior_reviews {
                s.push_str(&format!(
                    "\n### {} ({}): {}\n{}\n",
                    r.reviewer_id,
                    r.verdict.label(),
                    r.summary,
                    r.findings
                        .iter()
                        .map(|f| format!("- [{}] {}: {}", f.severity, f.category, f.description))
                        .collect::<Vec<_>>()
                        .join("\n")
                ));
            }
            s
        };

        format!(
            r#"You are a senior software architect performing a code review.

## MANDATORY: Read Architecture Context FIRST
You MUST read the architecture context below BEFORE reviewing any code.
Every review judgment MUST reference the architecture to justify your assessment.

## Architecture Context (READ THIS FIRST)
{arch}

## PR Information
Title: {title}
Description: {desc}

## Changed Files
{files}

## Code Diff
```diff
{diff}
```
{prior}

## Instructions
Review this PR against the architecture context above. For each issue found,
reference the relevant architecture section. Respond in EXACTLY this JSON format:

```json
{{
  "summary": "one-line summary of what this PR does",
  "verdict": "approve" | "request_changes" | "comment",
  "architecture_alignment": "assessment of how well this aligns with project architecture",
  "findings": [
    {{
      "severity": "critical" | "high" | "medium" | "low",
      "category": "architecture" | "efficiency" | "security" | "correctness" | "style",
      "file_path": "path/to/file.rs or null",
      "line_range": [start, end] or null,
      "description": "what the issue is",
      "suggestion": "how to fix it or null"
    }}
  ]
}}
```

Focus on substantive issues: architecture violations, logic errors, security problems,
efficiency issues. Skip trivial style nitpicks. If the code is good, say so."#,
            arch = ctx.architecture_context,
            title = ctx.title,
            desc = ctx.description,
            files = ctx.changed_files.join("\n"),
            diff = ctx.diff,
            prior = prior_summary,
        )
    }
}

/// Response structure from Gemini for parsing.
#[derive(Debug, Deserialize)]
struct GeminiReviewResponse {
    summary: String,
    verdict: String,
    architecture_alignment: Option<String>,
    #[serde(default)]
    findings: Vec<GeminiFinding>,
}

#[derive(Debug, Deserialize)]
struct GeminiFinding {
    severity: String,
    category: String,
    file_path: Option<String>,
    line_range: Option<Vec<usize>>,
    description: String,
    suggestion: Option<String>,
}

impl GeminiFinding {
    fn into_review_finding(self) -> ReviewFinding {
        ReviewFinding {
            severity: match self.severity.as_str() {
                "critical" => Severity::Critical,
                "high" => Severity::High,
                "medium" => Severity::Medium,
                _ => Severity::Low,
            },
            file_path: self.file_path,
            line_range: self.line_range.and_then(|v| {
                if v.len() >= 2 {
                    Some((v[0], v[1]))
                } else {
                    None
                }
            }),
            category: self.category,
            description: self.description,
            suggestion: self.suggestion,
        }
    }
}

#[async_trait]
impl CodeReviewer for GeminiReviewer {
    fn id(&self) -> &str {
        &self.id
    }

    fn model_name(&self) -> &str {
        &self.model
    }

    async fn review(&self, ctx: &ReviewContext) -> anyhow::Result<ReviewReport> {
        let start = Instant::now();
        let prompt = Self::build_prompt(ctx);

        // Build Gemini API request
        let payload = serde_json::json!({
            "contents": [{
                "parts": [{ "text": prompt }]
            }],
            "generationConfig": {
                "temperature": 0.2,
                "maxOutputTokens": 4096,
                "responseMimeType": "application/json"
            }
        });

        let url = format!("{}?key={}", self.endpoint, self.api_key);
        let resp = self
            .client
            .post(&url)
            .header("x-goog-api-key", &self.api_key)
            .json(&payload)
            .timeout(std::time::Duration::from_secs(120))
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Gemini API error {}: {}", status, body);
        }

        let body: serde_json::Value = resp.json().await?;
        let text = body["candidates"][0]["content"]["parts"][0]["text"]
            .as_str()
            .unwrap_or("{}");

        // Parse the JSON response
        let parsed: GeminiReviewResponse =
            serde_json::from_str(text).unwrap_or_else(|_| GeminiReviewResponse {
                summary: format!("Raw review: {}", &text[..text.len().min(200)]),
                verdict: "comment".into(),
                architecture_alignment: None,
                findings: vec![],
            });

        let verdict = match parsed.verdict.as_str() {
            "approve" => ReviewVerdict::Approve,
            "request_changes" => ReviewVerdict::RequestChanges,
            _ => ReviewVerdict::Comment,
        };

        Ok(ReviewReport {
            reviewer_id: self.id.clone(),
            model: self.model.clone(),
            summary: parsed.summary,
            verdict,
            findings: parsed
                .findings
                .into_iter()
                .map(GeminiFinding::into_review_finding)
                .collect(),
            architecture_alignment: parsed.architecture_alignment,
            duration_ms: start.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
        })
    }
}

// ── Claude reviewer (implementation quality) ─────────────────────

/// Code reviewer powered by Anthropic Claude API.
///
/// Acts as the implementation quality checker, focusing on code
/// correctness, efficiency, and adherence to coding standards.
pub struct ClaudeReviewer {
    /// Reviewer identifier.
    id: String,
    /// Anthropic API key.
    api_key: String,
    /// Model to use (e.g. "claude-sonnet-4-6", "claude-opus-4-6").
    model: String,
    /// HTTP client.
    client: reqwest::Client,
}

impl ClaudeReviewer {
    /// Create a new Claude reviewer.
    pub fn new(api_key: String, model: String) -> Self {
        Self {
            id: format!("claude-{}", model.split('-').nth(1).unwrap_or("reviewer")),
            api_key,
            model,
            client: reqwest::Client::new(),
        }
    }

    /// Build the review prompt for Claude.
    fn build_prompt(ctx: &ReviewContext) -> String {
        let prior_summary = if ctx.prior_reviews.is_empty() {
            String::new()
        } else {
            let mut s = String::from("\n\n## Prior Review Reports (from other reviewers)\n");
            for r in &ctx.prior_reviews {
                s.push_str(&format!(
                    "\n### {} ({}) — {}: {}\n",
                    r.reviewer_id,
                    r.model,
                    r.verdict.label(),
                    r.summary,
                ));
                for f in &r.findings {
                    s.push_str(&format!(
                        "- [{}] {}: {}",
                        f.severity, f.category, f.description,
                    ));
                    if let Some(ref sug) = f.suggestion {
                        s.push_str(&format!(" → Suggestion: {}", sug));
                    }
                    s.push('\n');
                }
                s.push_str("\nEvaluate whether these findings are valid. Accept correct findings ");
                s.push_str("and push back on incorrect ones with reasoning.\n");
            }
            s
        };

        format!(
            r#"You are an expert code reviewer evaluating a pull request.

## Architecture Context
{arch}

## PR Information
Title: {title}
Description: {desc}

## Changed Files
{files}

## Code Diff
```diff
{diff}
```
{prior}

## Instructions
Review this code for correctness, efficiency, and quality.
If prior reviews exist, evaluate their findings and provide your own assessment.

Respond in EXACTLY this JSON format:
```json
{{
  "summary": "one-line summary",
  "verdict": "approve" | "request_changes" | "comment",
  "architecture_alignment": "assessment or null",
  "findings": [
    {{
      "severity": "critical" | "high" | "medium" | "low",
      "category": "correctness" | "efficiency" | "security" | "architecture" | "style",
      "file_path": "path or null",
      "line_range": [start, end] or null,
      "description": "issue description",
      "suggestion": "fix suggestion or null"
    }}
  ]
}}
```"#,
            arch = ctx.architecture_context,
            title = ctx.title,
            desc = ctx.description,
            files = ctx.changed_files.join("\n"),
            diff = ctx.diff,
            prior = prior_summary,
        )
    }
}

#[async_trait]
impl CodeReviewer for ClaudeReviewer {
    fn id(&self) -> &str {
        &self.id
    }

    fn model_name(&self) -> &str {
        &self.model
    }

    async fn review(&self, ctx: &ReviewContext) -> anyhow::Result<ReviewReport> {
        let start = Instant::now();
        let prompt = Self::build_prompt(ctx);

        let payload = serde_json::json!({
            "model": self.model,
            "max_tokens": 4096,
            "temperature": 0.2,
            "messages": [{
                "role": "user",
                "content": prompt,
            }]
        });

        let resp = self
            .client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&payload)
            .timeout(std::time::Duration::from_secs(120))
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Claude API error {}: {}", status, body);
        }

        let body: serde_json::Value = resp.json().await?;
        let text = body["content"][0]["text"].as_str().unwrap_or("{}");

        // Extract JSON from potential markdown code block
        let json_str = extract_json_block(text);

        let parsed: GeminiReviewResponse =
            serde_json::from_str(json_str).unwrap_or_else(|_| GeminiReviewResponse {
                summary: format!("Raw review: {}", &text[..text.len().min(200)]),
                verdict: "comment".into(),
                architecture_alignment: None,
                findings: vec![],
            });

        let verdict = match parsed.verdict.as_str() {
            "approve" => ReviewVerdict::Approve,
            "request_changes" => ReviewVerdict::RequestChanges,
            _ => ReviewVerdict::Comment,
        };

        Ok(ReviewReport {
            reviewer_id: self.id.clone(),
            model: self.model.clone(),
            summary: parsed.summary,
            verdict,
            findings: parsed
                .findings
                .into_iter()
                .map(GeminiFinding::into_review_finding)
                .collect(),
            architecture_alignment: parsed.architecture_alignment,
            duration_ms: start.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
        })
    }
}

// ── Helper: extract JSON from markdown code blocks ───────────────

/// Extract JSON content from a response that may be wrapped in ```json blocks.
fn extract_json_block(text: &str) -> &str {
    // Try to find ```json ... ``` block
    if let Some(start) = text.find("```json") {
        let json_start = start + 7; // skip "```json"
        if let Some(end) = text[json_start..].find("```") {
            return text[json_start..json_start + end].trim();
        }
    }
    // Try plain ``` block
    if let Some(start) = text.find("```") {
        let block_start = start + 3;
        if let Some(end) = text[block_start..].find("```") {
            let candidate = text[block_start..block_start + end].trim();
            // Skip the language identifier line if present
            if let Some(nl) = candidate.find('\n') {
                let first_line = &candidate[..nl];
                if !first_line.starts_with('{') {
                    return candidate[nl + 1..].trim();
                }
            }
            return candidate;
        }
    }
    // Return as-is (hopefully raw JSON)
    text.trim()
}

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_json_from_markdown() {
        let input = r#"Here's my review:
```json
{"summary": "test", "verdict": "approve", "findings": []}
```"#;
        let extracted = extract_json_block(input);
        assert!(extracted.starts_with('{'));
        let parsed: serde_json::Value = serde_json::from_str(extracted).unwrap();
        assert_eq!(parsed["verdict"], "approve");
    }

    #[test]
    fn extract_json_from_plain_block() {
        let input = r#"```
{"summary": "test", "verdict": "comment", "findings": []}
```"#;
        let extracted = extract_json_block(input);
        assert!(extracted.starts_with('{'));
    }

    #[test]
    fn extract_raw_json() {
        let input = r#"{"summary": "test", "verdict": "approve", "findings": []}"#;
        let extracted = extract_json_block(input);
        assert_eq!(extracted, input);
    }

    #[test]
    fn gemini_finding_conversion() {
        let finding = GeminiFinding {
            severity: "critical".into(),
            category: "security".into(),
            file_path: Some("src/main.rs".into()),
            line_range: Some(vec![10, 20]),
            description: "SQL injection".into(),
            suggestion: Some("Use parameterized queries".into()),
        };
        let converted = finding.into_review_finding();
        assert_eq!(converted.severity, Severity::Critical);
        assert_eq!(converted.line_range, Some((10, 20)));
    }

    #[test]
    fn gemini_reviewer_prompt_includes_context() {
        let ctx = ReviewContext {
            diff: "+fn main() {}".into(),
            changed_files: vec!["src/main.rs".into()],
            architecture_context: "Trait-driven architecture".into(),
            title: "feat: add feature".into(),
            description: "Adds a new feature".into(),
            prior_reviews: vec![],
        };
        let prompt = GeminiReviewer::build_prompt(&ctx);
        assert!(prompt.contains("Trait-driven architecture"));
        assert!(prompt.contains("feat: add feature"));
        assert!(prompt.contains("+fn main() {}"));
    }

    #[test]
    fn claude_reviewer_prompt_includes_prior_reviews() {
        let prior = ReviewReport {
            reviewer_id: "gemini-flash".into(),
            model: "gemini-2.5-flash".into(),
            summary: "Found issues".into(),
            verdict: ReviewVerdict::RequestChanges,
            findings: vec![ReviewFinding {
                severity: Severity::High,
                file_path: Some("src/lib.rs".into()),
                line_range: None,
                category: "architecture".into(),
                description: "Violates SRP".into(),
                suggestion: None,
            }],
            architecture_alignment: None,
            duration_ms: 100,
        };

        let ctx = ReviewContext {
            diff: "+code".into(),
            changed_files: vec!["src/lib.rs".into()],
            architecture_context: "KISS".into(),
            title: "fix: something".into(),
            description: "Fix".into(),
            prior_reviews: vec![prior],
        };
        let prompt = ClaudeReviewer::build_prompt(&ctx);
        assert!(prompt.contains("gemini-flash"));
        assert!(prompt.contains("Violates SRP"));
        assert!(prompt.contains("Evaluate whether these findings are valid"));
    }
}
