//! Feed executor — runs feed handlers and manages feed item persistence.
//!
//! Strict mode:
//! - Only code handlers are supported.
//! - Handlers must emit valid items of the canonical 24 feed card types.
//! - No URL-feed support, no legacy/backward compatibility fallbacks.

use crate::aria::db::AriaDb;
use crate::aria::types::{FeedCardType, FeedItem, FeedResult};
use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::params;
use uuid::Uuid;

/// Executes feed handlers and manages feed item storage/retention.
pub struct FeedExecutor {
    db: AriaDb,
}

impl FeedExecutor {
    pub fn new(db: AriaDb) -> Self {
        Self { db }
    }

    fn shell_single_quote(input: &str) -> String {
        let escaped = input.replace('\'', r#"'\''"#);
        format!("'{escaped}'")
    }

    /// Execute a feed's handler and return the result (strict code-only mode).
    pub async fn execute(
        &self,
        feed_id: &str,
        tenant_id: &str,
        handler_code: &str,
        run_id: &str,
    ) -> Result<FeedResult> {
        tracing::info!(
            feed_id = feed_id,
            tenant_id = tenant_id,
            run_id = run_id,
            "Executing feed handler"
        );

        // Empty handler code is always an error.
        if handler_code.is_empty() {
            return Ok(FeedResult {
                success: false,
                items: Vec::new(),
                summary: None,
                metadata: None,
                error: Some("Empty handler code".to_string()),
            });
        }

        // Code handler: dispatch to Quilt container runtime.
        Self::execute_code_handler(feed_id, tenant_id, handler_code, run_id).await
    }

    /// Execute a code-based feed handler via the Quilt container runtime.
    ///
    /// Creates a sandboxed container, injects the handler code, executes it,
    /// and parses the output as feed items.
    async fn execute_code_handler(
        feed_id: &str,
        tenant_id: &str,
        handler_code: &str,
        run_id: &str,
    ) -> Result<FeedResult> {
        use crate::quilt::client::QuiltClient;
        const RESULT_MARKER: &str = "__ARIA_FEED_RESULT__";

        fn handler_code_sanitize(src: &str) -> &str {
            // SDK may append `// Handler method:` with a raw class-method snippet
            // (e.g. `async fetch(ctx){...}`) which is invalid at top-level JS.
            src.split("\n// Handler method:\n").next().unwrap_or(src)
        }

        fn extract_class_name(src: &str) -> Option<String> {
            let s = src;
            let idx = s.find("class ")?;
            let rest = &s[idx + "class ".len()..];
            let name = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '$')
                .collect::<String>();
            if name.is_empty() { None } else { Some(name) }
        }

        fn parse_card_type(s: &str) -> Result<FeedCardType> {
            // FeedCardType is serde snake_case, so parsing from JSON string is easiest.
            let json = format!("\"{s}\"");
            serde_json::from_str::<FeedCardType>(&json)
                .with_context(|| format!("Unknown cardType '{s}'"))
        }

        fn require_obj<'a>(
            v: &'a serde_json::Value,
            ctx: &str,
        ) -> Result<&'a serde_json::Map<String, serde_json::Value>> {
            v.as_object().with_context(|| format!("{ctx} must be an object"))
        }

        fn require_str<'a>(
            obj: &'a serde_json::Map<String, serde_json::Value>,
            k: &str,
            ctx: &str,
        ) -> Result<&'a str> {
            obj.get(k)
                .and_then(|v| v.as_str())
                .with_context(|| format!("{ctx}.{k} must be a string"))
        }

        fn require_num(
            obj: &serde_json::Map<String, serde_json::Value>,
            k: &str,
            ctx: &str,
        ) -> Result<()> {
            obj.get(k)
                .and_then(|v| v.as_f64())
                .with_context(|| format!("{ctx}.{k} must be a number"))?;
            Ok(())
        }

        fn require_int(
            obj: &serde_json::Map<String, serde_json::Value>,
            k: &str,
            ctx: &str,
        ) -> Result<()> {
            obj.get(k)
                .and_then(|v| v.as_i64())
                .with_context(|| format!("{ctx}.{k} must be an integer"))?;
            Ok(())
        }

        fn validate_metadata(card_type: &FeedCardType, meta: &serde_json::Value) -> Result<()> {
            use FeedCardType::*;
            let obj = require_obj(meta, "metadata")?;
            match card_type {
                Stock => {
                    require_str(obj, "ticker", "metadata")?;
                    require_str(obj, "name", "metadata")?;
                    require_num(obj, "price", "metadata")?;
                    require_num(obj, "change", "metadata")?;
                    require_num(obj, "changePercent", "metadata")?;
                    obj.get("sparkline")
                        .and_then(|v| v.as_array())
                        .filter(|a| !a.is_empty())
                        .with_context(|| "metadata.sparkline must be a non-empty array")?;
                }
                Crypto => {
                    require_str(obj, "symbol", "metadata")?;
                    require_str(obj, "name", "metadata")?;
                    require_num(obj, "price", "metadata")?;
                    require_num(obj, "change24h", "metadata")?;
                    require_num(obj, "changePercent24h", "metadata")?;
                    require_num(obj, "volume24h", "metadata")?;
                    require_num(obj, "marketCap", "metadata")?;
                    obj.get("sparkline")
                        .and_then(|v| v.as_array())
                        .filter(|a| !a.is_empty())
                        .with_context(|| "metadata.sparkline must be a non-empty array")?;
                }
                Prediction => {
                    require_str(obj, "id", "metadata")?;
                    require_str(obj, "question", "metadata")?;
                    require_num(obj, "yesPrice", "metadata")?;
                    require_num(obj, "volume", "metadata")?;
                    require_str(obj, "category", "metadata")?;
                    require_str(obj, "endDate", "metadata")?;
                    require_str(obj, "source", "metadata")?;
                }
                Game => {
                    require_str(obj, "id", "metadata")?;
                    require_str(obj, "league", "metadata")?;
                    require_str(obj, "teamA", "metadata")?;
                    require_str(obj, "teamB", "metadata")?;
                    if obj.get("scoreA").is_none() || obj.get("scoreB").is_none() {
                        anyhow::bail!("metadata.scoreA and metadata.scoreB are required");
                    }
                    require_str(obj, "status", "metadata")?;
                    require_str(obj, "detail", "metadata")?;
                }
                News => {
                    require_str(obj, "id", "metadata")?;
                    require_str(obj, "headline", "metadata")?;
                    require_str(obj, "source", "metadata")?;
                    require_str(obj, "category", "metadata")?;
                    require_str(obj, "timestamp", "metadata")?;
                }
                Social => {
                    require_str(obj, "id", "metadata")?;
                    require_str(obj, "handle", "metadata")?;
                    require_str(obj, "displayName", "metadata")?;
                    require_str(obj, "content", "metadata")?;
                    require_int(obj, "likes", "metadata")?;
                    require_int(obj, "reposts", "metadata")?;
                    require_str(obj, "timestamp", "metadata")?;
                    require_str(obj, "platform", "metadata")?;
                }
                Poll => {
                    require_str(obj, "id", "metadata")?;
                    require_str(obj, "question", "metadata")?;
                    obj.get("options")
                        .and_then(|v| v.as_array())
                        .filter(|a| a.len() >= 2)
                        .with_context(|| "metadata.options must be an array with at least 2 items")?;
                    require_int(obj, "totalVotes", "metadata")?;
                }
                Chart => {
                    require_str(obj, "id", "metadata")?;
                    require_str(obj, "title", "metadata")?;
                    require_str(obj, "chartType", "metadata")?;
                    obj.get("data")
                        .and_then(|v| v.as_array())
                        .filter(|a| !a.is_empty())
                        .with_context(|| "metadata.data must be a non-empty array")?;
                }
                Logs => {
                    require_str(obj, "id", "metadata")?;
                    require_str(obj, "title", "metadata")?;
                    obj.get("entries")
                        .and_then(|v| v.as_array())
                        .filter(|a| !a.is_empty())
                        .with_context(|| "metadata.entries must be a non-empty array")?;
                }
                Table => {
                    require_str(obj, "id", "metadata")?;
                    require_str(obj, "title", "metadata")?;
                    obj.get("columns")
                        .and_then(|v| v.as_array())
                        .filter(|a| !a.is_empty())
                        .with_context(|| "metadata.columns must be a non-empty array")?;
                    obj.get("rows")
                        .and_then(|v| v.as_array())
                        .filter(|a| !a.is_empty())
                        .with_context(|| "metadata.rows must be a non-empty array")?;
                }
                Kv => {
                    require_str(obj, "id", "metadata")?;
                    require_str(obj, "title", "metadata")?;
                    obj.get("pairs")
                        .and_then(|v| v.as_array())
                        .filter(|a| !a.is_empty())
                        .with_context(|| "metadata.pairs must be a non-empty array")?;
                }
                Metric => {
                    require_str(obj, "id", "metadata")?;
                    require_str(obj, "label", "metadata")?;
                    require_num(obj, "value", "metadata")?;
                }
                Code => {
                    require_str(obj, "id", "metadata")?;
                    require_str(obj, "title", "metadata")?;
                    require_str(obj, "language", "metadata")?;
                    require_str(obj, "code", "metadata")?;
                }
                Integration => {
                    require_str(obj, "id", "metadata")?;
                    require_str(obj, "name", "metadata")?;
                    require_str(obj, "type", "metadata")?;
                    require_str(obj, "status", "metadata")?;
                    require_str(obj, "lastSync", "metadata")?;
                    obj.get("metrics")
                        .and_then(|v| v.as_array())
                        .with_context(|| "metadata.metrics must be an array")?;
                }
                Weather => {
                    require_str(obj, "id", "metadata")?;
                    require_str(obj, "location", "metadata")?;
                    require_num(obj, "temp", "metadata")?;
                    require_num(obj, "feelsLike", "metadata")?;
                    require_str(obj, "condition", "metadata")?;
                    require_num(obj, "humidity", "metadata")?;
                    require_num(obj, "wind", "metadata")?;
                    obj.get("forecast")
                        .and_then(|v| v.as_array())
                        .filter(|a| !a.is_empty())
                        .with_context(|| "metadata.forecast must be a non-empty array")?;
                }
                Calendar => {
                    require_str(obj, "id", "metadata")?;
                    require_str(obj, "date", "metadata")?;
                    obj.get("events")
                        .and_then(|v| v.as_array())
                        .with_context(|| "metadata.events must be an array")?;
                }
                Flight => {
                    require_str(obj, "id", "metadata")?;
                    require_str(obj, "flightNumber", "metadata")?;
                    require_str(obj, "airline", "metadata")?;
                    obj.get("departure")
                        .and_then(|v| v.as_object())
                        .with_context(|| "metadata.departure must be an object")?;
                    obj.get("arrival")
                        .and_then(|v| v.as_object())
                        .with_context(|| "metadata.arrival must be an object")?;
                    require_str(obj, "status", "metadata")?;
                }
                Ci => {
                    require_str(obj, "id", "metadata")?;
                    require_str(obj, "repo", "metadata")?;
                    require_str(obj, "branch", "metadata")?;
                    require_str(obj, "commit", "metadata")?;
                    require_str(obj, "status", "metadata")?;
                    require_str(obj, "author", "metadata")?;
                    require_str(obj, "message", "metadata")?;
                    obj.get("stages")
                        .and_then(|v| v.as_array())
                        .filter(|a| !a.is_empty())
                        .with_context(|| "metadata.stages must be a non-empty array")?;
                }
                Github => {
                    require_str(obj, "id", "metadata")?;
                    require_str(obj, "repo", "metadata")?;
                    obj.get("events")
                        .and_then(|v| v.as_array())
                        .filter(|a| !a.is_empty())
                        .with_context(|| "metadata.events must be a non-empty array")?;
                }
                Image => {
                    require_str(obj, "id", "metadata")?;
                    require_str(obj, "title", "metadata")?;
                    require_str(obj, "url", "metadata")?;
                }
                Video => {
                    require_str(obj, "id", "metadata")?;
                    require_str(obj, "title", "metadata")?;
                    require_str(obj, "url", "metadata")?;
                }
                Audio => {
                    require_str(obj, "id", "metadata")?;
                    require_str(obj, "title", "metadata")?;
                    require_str(obj, "url", "metadata")?;
                }
                Webview => {
                    require_str(obj, "id", "metadata")?;
                    require_str(obj, "title", "metadata")?;
                    require_str(obj, "url", "metadata")?;
                }
                File => {
                    require_str(obj, "id", "metadata")?;
                    require_str(obj, "name", "metadata")?;
                    require_str(obj, "extension", "metadata")?;
                    require_str(obj, "contentType", "metadata")?;
                    require_int(obj, "size", "metadata")?;
                    require_str(obj, "createdAt", "metadata")?;
                    if obj.get("updatedAt").is_none()
                        || obj.get("description").is_none()
                        || obj.get("tags").is_none()
                    {
                        anyhow::bail!("metadata.updatedAt, metadata.description, and metadata.tags are required");
                    }
                }
            }
            Ok(())
        }

        // Attempt to connect to Quilt runtime
        let quilt = match QuiltClient::from_env() {
            Ok(client) => client,
            Err(_) => {
                return Ok(FeedResult {
                    success: false,
                    items: Vec::new(),
                    summary: None,
                    metadata: None,
                    error: Some(
                        "Code handler execution requires a running Quilt container runtime. \
                         Set QUILT_API_URL and QUILT_API_KEY."
                            .to_string(),
                    ),
                });
            }
        };

        // Prefer the long-lived `aria-exec` container (it has Node installed).
        // In some Quilt environments, newly-created containers may not have Node by default.
        let containers = match quilt.list_containers().await {
            Ok(c) => c,
            Err(e) => {
                return Ok(FeedResult {
                    success: false,
                    items: Vec::new(),
                    summary: None,
                    metadata: None,
                    error: Some(format!("Failed to list Quilt containers: {e}")),
                });
            }
        };

        let container = containers
            .iter()
            .find(|c| c.name == "aria-exec")
            .cloned()
            .or_else(|| {
                containers
                    .iter()
                    .find(|c| c.state == crate::quilt::client::QuiltContainerState::Running)
                    .cloned()
            });

        let Some(container) = container else {
            return Ok(FeedResult {
                success: false,
                items: Vec::new(),
                summary: None,
                metadata: None,
                error: Some("No usable Quilt container found (expected 'aria-exec')".into()),
            });
        };

        if container.state != crate::quilt::client::QuiltContainerState::Running {
            if let Err(e) = quilt.start_container(&container.id).await {
                return Ok(FeedResult {
                    success: false,
                    items: Vec::new(),
                    summary: None,
                    metadata: None,
                    error: Some(format!("Failed to start Quilt container: {e}")),
                });
            }
        }

        // Execute strict wrapper inside the container
        let handler_clean = handler_code_sanitize(handler_code);
        let Some(class_name) = extract_class_name(handler_clean) else {
            return Ok(FeedResult {
                success: false,
                items: Vec::new(),
                summary: None,
                metadata: None,
                error: Some("Feed handler class not found (no `class Name` in handler_code)".into()),
            });
        };

        let ctx_json = serde_json::json!({
            "feed_id": feed_id.to_string(),
            "tenant_id": tenant_id.to_string(),
            "last_run_at": chrono::Utc::now().timestamp_millis(),
        })
        .to_string();

        let script = format!(
            r#"'use strict';
const RESULT_MARKER = '{marker}';
const __ctx = {ctx_json};

function nowIso() {{ return new Date().toISOString(); }}
function isoFromEpochSeconds(sec) {{ return new Date(Math.floor(Number(sec) * 1000)).toISOString(); }}
function stripHtml(s) {{
  if (typeof s !== "string") return "";
  return s.replace(/<[^>]*>/g, " ").replace(/\s+/g, " ").trim();
}}
function clamp(n, lo, hi) {{
  const x = Number(n);
  if (!Number.isFinite(x)) return lo;
  return Math.max(lo, Math.min(hi, x));
}}
function sparklineFromSeries(values, n = 30) {{
  const xs = Array.isArray(values) ? values.map(Number).filter(Number.isFinite) : [];
  if (xs.length === 0) throw new Error("sparkline series empty");
  if (xs.length <= n) return xs;
  const step = xs.length / n;
  const out = [];
  for (let i = 0; i < n; i++) out.push(xs[Math.floor(i * step)]);
  return out;
}}
function hashString(s) {{
  let h = 2166136261;
  for (let i = 0; i < s.length; i++) {{
    h ^= s.charCodeAt(i);
    h = Math.imul(h, 16777619);
  }}
  return h >>> 0;
}}
function parseCsvRows(csv) {{
  const lines = String(csv).trim().split(/\r?\n/).filter(Boolean);
  if (lines.length < 2) throw new Error("CSV has no rows");
  const headers = lines[0].split(",").map((h) => h.trim());
  const rows = [];
  for (const line of lines.slice(1)) {{
    const cols = line.split(",");
    const row = {{}};
    for (let i = 0; i < headers.length; i++) row[headers[i]] = (cols[i] ?? "").trim();
    rows.push(row);
  }}
  return rows;
}}
async function fetchJson(url, {{ timeoutMs = 8000, headers }} = {{}}) {{
  const controller = new AbortController();
  const id = setTimeout(() => controller.abort(), timeoutMs);
  try {{
    const res = await fetch(url, {{
      headers: {{ "User-Agent": "aria-feed/1.0", ...(headers ?? {{}}) }},
      signal: controller.signal,
    }});
    if (!res.ok) throw new Error(`HTTP ${{res.status}} ${{res.statusText}}`);
    return await res.json();
  }} finally {{
    clearTimeout(id);
  }}
}}
async function fetchText(url, {{ timeoutMs = 8000, headers }} = {{}}) {{
  const controller = new AbortController();
  const id = setTimeout(() => controller.abort(), timeoutMs);
  try {{
    const res = await fetch(url, {{
      headers: {{ "User-Agent": "aria-feed/1.0", ...(headers ?? {{}}) }},
      signal: controller.signal,
    }});
    if (!res.ok) throw new Error(`HTTP ${{res.status}} ${{res.statusText}}`);
    return await res.text();
  }} finally {{
    clearTimeout(id);
  }}
}}

{handler}

function __normalize(raw) {{
  if (Array.isArray(raw)) return {{ success: true, items: raw }};
  if (raw && typeof raw === 'object') {{
    return {{
      success: typeof raw.success === 'boolean' ? raw.success : true,
      items: Array.isArray(raw.items) ? raw.items : [],
      summary: typeof raw.summary === 'string' ? raw.summary : undefined,
      metadata: raw.metadata,
      error: typeof raw.error === 'string' ? raw.error : undefined,
    }};
  }}
  return {{ success: true, items: [] }};
}}

async function __run() {{
  const candidates = ['handler','execute','fetch','run'];
  const C = {class_name};
  const inst = new C();
  for (const name of candidates) {{
    if (typeof inst?.[name] === 'function') {{
      const out = await inst[name](__ctx);
      return __normalize(out);
    }}
  }}
  throw new Error('Feed handler method not found (expected one of: handler/execute/fetch/run)');
}}

(async () => {{
  try {{
    const out = await __run();
    console.log(RESULT_MARKER + JSON.stringify({{ success: true, result: out }}));
  }} catch (err) {{
    console.log(RESULT_MARKER + JSON.stringify({{
      success: false,
      error: err?.message || String(err),
      stack: err?.stack,
    }}));
    process.exit(1);
  }}
}})();
"#,
            marker = RESULT_MARKER,
            ctx_json = ctx_json,
            handler = handler_clean,
            class_name = class_name,
        );

        let script_path = format!("/tmp/aria-feed-{feed_id}-{run_id}.js");
        let eof = format!("ARIA_FEED_SCRIPT_EOF_{run_id}");
        let write_cmd = format!("cat > {script_path} << '{eof}'\n{script}\n{eof}");

        // Write script to container
        let write_result = quilt
            .exec(
                &container.id,
                crate::quilt::client::QuiltExecParams {
                    command: crate::quilt::client::QuiltExecCommand::Vec(vec![
                        "sh".into(),
                        "-c".into(),
                        write_cmd,
                    ]),
                    workdir: None,
                    capture_output: Some(true),
                    timeout_ms: Some(10_000),
                    detach: Some(false),
                },
            )
            .await;
        if let Err(e) = write_result {
            return Ok(FeedResult {
                success: false,
                items: Vec::new(),
                summary: None,
                metadata: None,
                error: Some(format!("Failed to write handler script: {e}")),
            });
        }

        // Execute script
        let exec_result = quilt
            .exec(
                &container.id,
                crate::quilt::client::QuiltExecParams {
                    command: crate::quilt::client::QuiltExecCommand::Vec(vec![
                        "node".into(),
                        script_path.clone(),
                    ]),
                    workdir: None,
                    capture_output: Some(true),
                    timeout_ms: Some(60_000),
                    detach: Some(false),
                },
            )
            .await;

        // Best-effort cleanup of script file
        let _ = quilt
            .exec(
                &container.id,
                crate::quilt::client::QuiltExecParams {
                    command: crate::quilt::client::QuiltExecCommand::Vec(vec![
                        "rm".into(),
                        "-f".into(),
                        script_path,
                    ]),
                    workdir: None,
                    capture_output: Some(true),
                    timeout_ms: Some(2_000),
                    detach: Some(false),
                },
            )
            .await;

        match exec_result {
            Ok(result) => {
                let idx = result
                    .stdout
                    .find(RESULT_MARKER)
                    .context("Feed handler did not emit __ARIA_FEED_RESULT__ marker")?;
                let json_str = result.stdout[idx + RESULT_MARKER.len()..]
                    .trim()
                    .lines()
                    .next()
                    .unwrap_or("");

                let parsed: serde_json::Value = match serde_json::from_str(json_str) {
                    Ok(v) => v,
                    Err(e) => {
                        let stdout_tail: String = result
                            .stdout
                            .chars()
                            .rev()
                            .take(500)
                            .collect::<String>()
                            .chars()
                            .rev()
                            .collect();
                        anyhow::bail!(
                            "Failed to parse feed marker JSON: {e}; marker_line={:?}; stdout_tail={:?}",
                            json_str,
                            stdout_tail
                        );
                    }
                };
                let ok = parsed.get("success").and_then(|v| v.as_bool()).unwrap_or(false);
                if !ok {
                    let err = parsed
                        .get("error")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Feed handler failed");
                    return Ok(FeedResult {
                        success: false,
                        items: Vec::new(),
                        summary: None,
                        metadata: None,
                        error: Some(err.to_string()),
                    });
                }

                if result.exit_code != 0 {
                    let stderr = result.stderr.trim();
                    let hint = if !stderr.is_empty() {
                        stderr.to_string()
                    } else {
                        // Wrapper prints structured error JSON to stdout before exiting non-zero.
                        "(no stderr)".to_string()
                    };
                    return Ok(FeedResult {
                        success: false,
                        items: Vec::new(),
                        summary: None,
                        metadata: None,
                        error: Some(format!("Handler exited with code {}: {hint}", result.exit_code)),
                    });
                }

                let result_obj = parsed.get("result").context("Missing result in marker")?;
                let res_obj = require_obj(result_obj, "result")?;
                let items = res_obj
                    .get("items")
                    .and_then(|v| v.as_array())
                    .context("result.items must be an array")?;

                let mut out_items: Vec<FeedItem> = Vec::new();
                for (i, item) in items.iter().enumerate() {
                    let item_obj = require_obj(item, &format!("items[{i}]"))?;
                    let ct = require_str(item_obj, "cardType", &format!("items[{i}]"))?;
                    let title = require_str(item_obj, "title", &format!("items[{i}]"))?.to_string();
                    let card_type = parse_card_type(ct).with_context(|| format!("items[{i}]"))?;

                    let meta = item_obj.get("metadata").context("items[].metadata is required")?;
                    validate_metadata(&card_type, meta)
                        .with_context(|| format!("items[{i}].metadata"))?;

                    let meta_obj = require_obj(meta, "metadata")?;
                    let meta_map: std::collections::HashMap<String, serde_json::Value> =
                        meta_obj.clone().into_iter().collect();

                    let body = item_obj.get("body").and_then(|v| v.as_str()).map(|s| s.to_string());
                    let source = item_obj.get("source").and_then(|v| v.as_str()).map(|s| s.to_string());
                    let url = item_obj.get("url").and_then(|v| v.as_str()).map(|s| s.to_string());
                    let ts = item_obj
                        .get("timestamp")
                        .and_then(|v| v.as_i64())
                        .unwrap_or_else(|| Utc::now().timestamp_millis());

                    out_items.push(FeedItem {
                        card_type,
                        title,
                        body,
                        source,
                        url,
                        metadata: Some(meta_map),
                        timestamp: Some(ts),
                    });
                }

                Ok(FeedResult {
                    success: res_obj.get("success").and_then(|v| v.as_bool()).unwrap_or(true),
                    items: out_items,
                    summary: res_obj.get("summary").and_then(|v| v.as_str()).map(|s| s.to_string()),
                    metadata: res_obj.get("metadata").cloned(),
                    error: res_obj.get("error").and_then(|v| v.as_str()).map(|s| s.to_string()),
                })
            }
            Err(e) => Ok(FeedResult {
                success: false,
                items: Vec::new(),
                summary: None,
                metadata: None,
                error: Some(format!("Handler execution failed: {e}")),
            }),
        }
    }

    /// Store feed items in the database.
    pub fn store_items(
        &self,
        tenant_id: &str,
        feed_id: &str,
        run_id: &str,
        items: &[FeedItem],
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "INSERT INTO aria_feed_items
                 (id, tenant_id, feed_id, run_id, card_type, title, body, source, url, metadata, timestamp, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            )?;

            for item in items {
                let id = Uuid::new_v4().to_string();
                let card_type = serde_json::to_value(&item.card_type)
                    .context("Failed to serialize FeedItem.card_type")?
                    .as_str()
                    .context("FeedItem.card_type must serialize to a string")?
                    .to_string();
                let meta = item
                    .metadata
                    .as_ref()
                    .context("FeedItem.metadata is required")?;
                let metadata_json = Some(
                    serde_json::to_string(meta).context("Failed to serialize FeedItem.metadata")?,
                );
                let ts = item.timestamp.context("FeedItem.timestamp is required")?;
                // The dashboard expects ms; auto-upconvert if it looks like seconds.
                let ts = if ts.abs() < 100_000_000_000 { ts * 1000 } else { ts };

                stmt.execute(params![
                    id,
                    tenant_id,
                    feed_id,
                    run_id,
                    card_type,
                    item.title,
                    item.body,
                    item.source,
                    item.url,
                    metadata_json,
                    ts,
                    now,
                ])?;
            }
            Ok(())
        })
    }

    /// Prune items older than retention policy.
    ///
    /// Returns the number of items deleted.
    /// - `max_items`: keep only this many most recent items per feed
    /// - `max_age_days`: remove items older than N days
    pub fn prune_by_retention(
        &self,
        feed_id: &str,
        max_items: Option<u32>,
        max_age_days: Option<u32>,
    ) -> Result<u64> {
        let mut total_pruned: u64 = 0;

        // Prune by max age first
        if let Some(days) = max_age_days {
            let cutoff = Utc::now()
                .checked_sub_signed(chrono::Duration::days(i64::from(days)))
                .context("Failed to compute age cutoff")?
                .to_rfc3339();

            let pruned = self.db.with_conn(|conn| {
                let deleted = conn.execute(
                    "DELETE FROM aria_feed_items WHERE feed_id = ?1 AND created_at < ?2",
                    params![feed_id, cutoff],
                )?;
                Ok(deleted as u64)
            })?;
            total_pruned += pruned;
        }

        // Prune by max items (keep most recent N)
        if let Some(max) = max_items {
            let pruned = self.db.with_conn(|conn| {
                // Delete items beyond the max count, keeping the most recent
                let deleted = conn.execute(
                    "DELETE FROM aria_feed_items WHERE feed_id = ?1 AND id NOT IN (
                        SELECT id FROM aria_feed_items WHERE feed_id = ?1
                        ORDER BY created_at DESC LIMIT ?2
                    )",
                    params![feed_id, max],
                )?;
                Ok(deleted as u64)
            })?;
            total_pruned += pruned;
        }

        if total_pruned > 0 {
            tracing::debug!(
                feed_id = feed_id,
                pruned = total_pruned,
                "Pruned feed items by retention policy"
            );
        }

        Ok(total_pruned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aria::db::AriaDb;
    use crate::aria::types::{FeedCardType, FeedItem};
    use std::collections::HashMap;

    fn setup() -> (AriaDb, FeedExecutor) {
        let db = AriaDb::open_in_memory().unwrap();
        let executor = FeedExecutor::new(db.clone());
        (db, executor)
    }

    fn sample_items(count: usize) -> Vec<FeedItem> {
        (0..count)
            .map(|i| FeedItem {
                card_type: FeedCardType::News,
                title: format!("Item {i}"),
                body: Some(format!("Body of item {i}")),
                source: Some("test".to_string()),
                url: Some(format!("https://example.com/{i}")),
                metadata: Some(HashMap::from([
                    ("id".to_string(), serde_json::json!(format!("n_{i}"))),
                    ("headline".to_string(), serde_json::json!(format!("Headline {i}"))),
                    ("source".to_string(), serde_json::json!("test")),
                    ("category".to_string(), serde_json::json!("test")),
                    ("timestamp".to_string(), serde_json::json!(Utc::now().to_rfc3339())),
                ])),
                timestamp: Some(Utc::now().timestamp_millis()),
            })
            .collect()
    }

    #[tokio::test]
    async fn execute_returns_error_for_empty_handler() {
        let (_db, executor) = setup();
        let result = executor
            .execute("feed-1", "tenant-1", "", "run-1")
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.is_some());
        assert!(result.error.unwrap().contains("Empty handler code"));
    }

    #[tokio::test]
    async fn execute_returns_quilt_error_for_code_handler() {
        let (_db, executor) = setup();
        let result = executor
            .execute("feed-1", "tenant-1", "console.log('hello')", "run-1")
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.items.is_empty());
        let error = result.error.as_deref().unwrap();
        assert!(
            error.contains("Quilt container runtime"),
            "Expected Quilt error message, got: {error}"
        );
    }

    #[test]
    fn store_items_persists_to_db() {
        let (db, executor) = setup();
        let items = sample_items(3);
        executor
            .store_items("tenant-1", "feed-1", "run-1", &items)
            .unwrap();

        let count: i64 = db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM aria_feed_items WHERE feed_id = 'feed-1'",
                    [],
                    |row| row.get(0),
                )
                .map_err(Into::into)
            })
            .unwrap();
        assert_eq!(count, 3);
    }

    #[test]
    fn store_items_records_correct_fields() {
        let (db, executor) = setup();
        let items = vec![FeedItem {
            card_type: FeedCardType::News,
            title: "Test News".to_string(),
            body: Some("A news body".to_string()),
            source: Some("reuters".to_string()),
            url: Some("https://example.com/news".to_string()),
            metadata: Some(HashMap::from([
                ("id".to_string(), serde_json::json!("news_1")),
                ("headline".to_string(), serde_json::json!("Test News")),
                ("source".to_string(), serde_json::json!("reuters")),
                ("category".to_string(), serde_json::json!("markets")),
                ("timestamp".to_string(), serde_json::json!("2025-01-01T00:00:00Z")),
            ])),
            timestamp: Some(1_700_000_000_000),
        }];
        executor.store_items("t1", "f1", "r1", &items).unwrap();

        db.with_conn(|conn| {
            let (title, card_type, source): (String, String, String) = conn.query_row(
                "SELECT title, card_type, source FROM aria_feed_items WHERE feed_id = 'f1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
            assert_eq!(title, "Test News");
            assert_eq!(card_type, "news");
            assert_eq!(source, "reuters");
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn store_empty_items_is_noop() {
        let (_db, executor) = setup();
        executor.store_items("t1", "f1", "r1", &[]).unwrap();
    }

    #[test]
    fn prune_by_max_items_keeps_most_recent() {
        let (db, executor) = setup();

        // Insert 5 items with staggered timestamps
        for i in 0..5 {
            let created = format!("2025-01-{:02}T00:00:00+00:00", i + 1);
            db.with_conn(|conn| {
                conn.execute(
                    "INSERT INTO aria_feed_items (id, tenant_id, feed_id, run_id, card_type, title, created_at)
                     VALUES (?1, 't1', 'f1', 'r1', 'news', ?2, ?3)",
                    params![format!("item-{i}"), format!("Item {i}"), created],
                )?;
                Ok(())
            })
            .unwrap();
        }

        let pruned = executor.prune_by_retention("f1", Some(3), None).unwrap();
        assert_eq!(pruned, 2);

        let remaining: i64 = db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM aria_feed_items WHERE feed_id = 'f1'",
                    [],
                    |row| row.get(0),
                )
                .map_err(Into::into)
            })
            .unwrap();
        assert_eq!(remaining, 3);
    }

    #[test]
    fn prune_by_max_age_removes_old_items() {
        let (db, executor) = setup();

        // Insert an old item and a recent item
        let old_date = "2020-01-01T00:00:00+00:00";
        let recent_date = Utc::now().to_rfc3339();

        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO aria_feed_items (id, tenant_id, feed_id, run_id, card_type, title, created_at)
                 VALUES ('old', 't1', 'f1', 'r1', 'news', 'Old Item', ?1)",
                params![old_date],
            )?;
            conn.execute(
                "INSERT INTO aria_feed_items (id, tenant_id, feed_id, run_id, card_type, title, created_at)
                 VALUES ('new', 't1', 'f1', 'r1', 'news', 'New Item', ?1)",
                params![recent_date],
            )?;
            Ok(())
        })
        .unwrap();

        let pruned = executor.prune_by_retention("f1", None, Some(30)).unwrap();
        assert_eq!(pruned, 1);

        let remaining: i64 = db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM aria_feed_items WHERE feed_id = 'f1'",
                    [],
                    |row| row.get(0),
                )
                .map_err(Into::into)
            })
            .unwrap();
        assert_eq!(remaining, 1);
    }

    #[test]
    fn prune_with_no_policy_is_noop() {
        let (db, executor) = setup();
        let items = sample_items(5);
        executor.store_items("t1", "f1", "r1", &items).unwrap();

        let pruned = executor.prune_by_retention("f1", None, None).unwrap();
        assert_eq!(pruned, 0);

        let remaining: i64 = db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM aria_feed_items WHERE feed_id = 'f1'",
                    [],
                    |row| row.get(0),
                )
                .map_err(Into::into)
            })
            .unwrap();
        assert_eq!(remaining, 5);
    }

    #[test]
    fn prune_nonexistent_feed_returns_zero() {
        let (_db, executor) = setup();
        let pruned = executor
            .prune_by_retention("nonexistent", Some(10), Some(7))
            .unwrap();
        assert_eq!(pruned, 0);
    }
}
