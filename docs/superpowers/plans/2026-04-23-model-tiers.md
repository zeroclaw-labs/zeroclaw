# Live Model Discovery + Tier-Based Switching — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace zeroclaw's hardcoded model list with a live catalog fetched from `adi-cliproxy`, let the agent pick models by tier (`chat` / `thinking` / `fast`), and fix the silent-hang on non-2xx responses that caused the 2026-04-23 13-hour outage.

**Architecture:** A new `ModelCatalogClient` in `zeroclaw-providers` fetches `/v1/models` from cliproxy with a 60-second in-memory cache. Tier mappings live in each zeroclaw's volume as `/zeroclaw-data/.zeroclaw/tiers.yaml`, seeded on first boot by the existing entrypoint. The `model_switch` tool gains two new actions (`list_tiers`, `set_tier`), rewires `list_models` to use the live catalog, and validates `set` against it. A separate change in `compatible.rs` converts silent chat-endpoint failures into structured errors that surface to the agent loop.

**Tech Stack:** Rust 2024 edition, `reqwest` 0.12 (already a dep), `tokio::sync::Mutex` for cache guard, `serde_yaml` for the tier config, `wiremock` 0.6 for HTTP tests (workspace-established).

---

## Architectural deviation from spec

The spec proposed a new `/v1/tiers` endpoint on cliproxy. cliproxy (eceasy/cli-proxy-api v6.9.31) exposes **no** custom-endpoint configuration hook and is a single-process Go binary; adding an endpoint would mean forking it or running a sidecar. Given the spec's single-provider assumption and the fact that there are only two zeroclaw instances, this plan **puts `tiers.yaml` on each zeroclaw's volume** instead. Promoting a new model as the `thinking` default then requires editing two files (one per persona) instead of one on cliproxy. Acceptable trade-off; no new infra. `/v1/models` is still fetched live from cliproxy.

## File structure

Files this plan creates:

- `crates/zeroclaw-providers/src/catalog.rs` — new `ModelCatalogClient` with TTL cache; owns `/v1/models` HTTP calls and YAML tier-mapping reads.
- `crates/zeroclaw-providers/tests/catalog_test.rs` — wiremock-backed tests for the catalog client.
- `crates/zeroclaw-runtime/tests/model_switch_test.rs` — integration test exercising the new `list_tiers` / `set_tier` actions against a mock catalog.
- `deploy/zeroclaw/tiers.seed.yaml` — shipped in the Docker image, seeded to `/zeroclaw-data/.zeroclaw/tiers.yaml` on first boot.
- `docs/reference/api/model-tiers.md` — short reference doc explaining tiers to developers.

Files this plan modifies:

- `crates/zeroclaw-providers/src/lib.rs` — add `pub mod catalog` and re-export `ModelCatalogClient`.
- `crates/zeroclaw-providers/Cargo.toml` — add `serde_yaml` workspace dep if not already present.
- `crates/zeroclaw-runtime/src/tools/model_switch.rs` — rewrite the tool to use `ModelCatalogClient`; add `list_tiers` + `set_tier` actions; remove the hardcoded model list.
- `crates/zeroclaw-runtime/src/tools/mod.rs` — update `ModelSwitchTool::new` signature consumers if construction changes.
- `crates/zeroclaw-providers/src/compatible.rs` — silent-hang fix: convert non-2xx chat-completions responses into structured `ProviderError` with status + body.
- `deploy/zeroclaw/Dockerfile` — `COPY deploy/zeroclaw/tiers.seed.yaml` into the image.
- `deploy/zeroclaw/entrypoint.sh` — seed `/zeroclaw-data/.zeroclaw/tiers.yaml` on first boot if missing.

## Branch setup

- [ ] **Step 0: Create a working branch off the current deploy branch**

```bash
cd c:/git/adi
git checkout -b feature/model-tiers
```

---

## Task 1: ModelCatalogClient — skeleton + failing test

Establishes the module, public types, and one HTTP-backed test before any implementation.

**Files:**
- Create: `crates/zeroclaw-providers/src/catalog.rs`
- Create: `crates/zeroclaw-providers/tests/catalog_test.rs`
- Modify: `crates/zeroclaw-providers/src/lib.rs`
- Modify: `crates/zeroclaw-providers/Cargo.toml`

- [ ] **Step 1: Check serde_yaml and wiremock availability, add if missing**

Run:
```bash
grep -n "serde_yaml\|wiremock" crates/zeroclaw-providers/Cargo.toml
```

If `serde_yaml` is missing from `[dependencies]`, add it:
```toml
serde_yaml = "0.9"
```

If `wiremock` is missing from `[dev-dependencies]`, add it:
```toml
[dev-dependencies]
wiremock = "0.6"
tokio = { workspace = true, features = ["macros", "rt-multi-thread"] }
```

Verify:
```bash
cargo check -p zeroclaw-providers
```
Expected: clean check, no new errors.

- [ ] **Step 2: Create the module file with type skeletons**

Create `crates/zeroclaw-providers/src/catalog.rs`:

```rust
//! Live model catalog client.
//!
//! Fetches the provider's `/v1/models` endpoint and resolves semantic tiers
//! (chat / thinking / fast) from a YAML file. Cached for 60 seconds per
//! process so the agent can switch models mid-conversation without
//! re-fetching on every call.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

const CATALOG_TTL: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelEntry {
    pub id: String,
    #[serde(default)]
    pub owned_by: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TierEntry {
    pub name: String,
    pub model: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Deserialize)]
struct ModelsResponse {
    data: Vec<ModelEntry>,
}

#[derive(Debug, Deserialize)]
struct TiersFile {
    tiers: Vec<TierEntry>,
}

#[derive(Debug, Default)]
struct CachedCatalog {
    models: Option<(Vec<ModelEntry>, Instant)>,
}

pub struct ModelCatalogClient {
    base_url: String,
    api_key: String,
    tiers_path: PathBuf,
    http: reqwest::Client,
    cache: Arc<Mutex<CachedCatalog>>,
}

impl ModelCatalogClient {
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>, tiers_path: impl Into<PathBuf>) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .connect_timeout(Duration::from_secs(5))
            .build()
            .context("building catalog HTTP client")?;
        Ok(Self {
            base_url: base_url.into(),
            api_key: api_key.into(),
            tiers_path: tiers_path.into(),
            http,
            cache: Arc::new(Mutex::new(CachedCatalog::default())),
        })
    }

    pub async fn list_models(&self) -> Result<Vec<ModelEntry>> {
        anyhow::bail!("not yet implemented")
    }

    pub async fn list_tiers(&self) -> Result<Vec<TierEntry>> {
        anyhow::bail!("not yet implemented")
    }

    pub async fn resolve_tier(&self, _tier: &str) -> Result<String> {
        anyhow::bail!("not yet implemented")
    }
}
```

- [ ] **Step 3: Wire the module into `lib.rs`**

Edit `crates/zeroclaw-providers/src/lib.rs`. Find the block of `pub mod` declarations around line 19–38. Add:

```rust
pub mod catalog;
```

alphabetically between `pub mod bedrock;` and `pub mod claude_code;`. Below the existing `pub use traits::{...}` block, add:

```rust
pub use catalog::{ModelCatalogClient, ModelEntry, TierEntry};
```

Verify:
```bash
cargo check -p zeroclaw-providers
```
Expected: clean check.

- [ ] **Step 4: Write the first failing test — list_models happy path**

Create `crates/zeroclaw-providers/tests/catalog_test.rs`:

```rust
use std::path::PathBuf;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};
use zeroclaw_providers::ModelCatalogClient;

fn tiers_yaml_path() -> PathBuf {
    // tests don't exercise tier resolution in this file; point at a path that
    // does not exist so any accidental read surfaces as an error.
    PathBuf::from("/nonexistent/tiers.yaml")
}

#[tokio::test]
async fn list_models_returns_catalog_from_endpoint() {
    let server = MockServer::start().await;
    let body = serde_json::json!({
        "data": [
            {"id": "claude-opus-4-7",   "object": "model", "owned_by": "anthropic"},
            {"id": "claude-sonnet-4-6", "object": "model", "owned_by": "anthropic"}
        ]
    });
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .and(header("Authorization", "Bearer test-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .expect(1)
        .mount(&server)
        .await;

    let base = format!("{}/v1", server.uri());
    let client = ModelCatalogClient::new(base, "test-key", tiers_yaml_path()).unwrap();
    let models = client.list_models().await.expect("list_models");

    let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, vec!["claude-opus-4-7", "claude-sonnet-4-6"]);
}
```

- [ ] **Step 5: Run the test and verify it fails**

Run:
```bash
cargo test -p zeroclaw-providers --test catalog_test list_models_returns_catalog_from_endpoint -- --nocapture
```
Expected: FAIL with `not yet implemented` in the error chain.

- [ ] **Step 6: Commit the skeleton**

```bash
git add crates/zeroclaw-providers/src/catalog.rs \
        crates/zeroclaw-providers/src/lib.rs \
        crates/zeroclaw-providers/tests/catalog_test.rs \
        crates/zeroclaw-providers/Cargo.toml
git commit -m "feat(providers): catalog client skeleton + failing test"
```

---

## Task 2: Implement list_models + 60s cache

**Files:**
- Modify: `crates/zeroclaw-providers/src/catalog.rs`
- Modify: `crates/zeroclaw-providers/tests/catalog_test.rs`

- [ ] **Step 1: Implement `list_models` against the HTTP endpoint with caching**

In `catalog.rs`, replace the `list_models` stub with:

```rust
pub async fn list_models(&self) -> Result<Vec<ModelEntry>> {
    {
        let cache = self.cache.lock().await;
        if let Some((models, fetched_at)) = &cache.models {
            if fetched_at.elapsed() < CATALOG_TTL {
                return Ok(models.clone());
            }
        }
    }

    let url = format!("{}/models", self.base_url.trim_end_matches('/'));
    let resp = self
        .http
        .get(&url)
        .bearer_auth(&self.api_key)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("model catalog fetch failed: status={status} body={body}");
    }

    let parsed: ModelsResponse = resp
        .json()
        .await
        .context("parsing /v1/models response")?;

    {
        let mut cache = self.cache.lock().await;
        cache.models = Some((parsed.data.clone(), Instant::now()));
    }

    Ok(parsed.data)
}
```

- [ ] **Step 2: Run the happy-path test and verify it passes**

```bash
cargo test -p zeroclaw-providers --test catalog_test list_models_returns_catalog_from_endpoint -- --nocapture
```
Expected: PASS.

- [ ] **Step 3: Add cache-hit test**

Append to `catalog_test.rs`:

```rust
#[tokio::test]
async fn list_models_uses_cache_on_second_call_within_ttl() {
    let server = MockServer::start().await;
    let body = serde_json::json!({
        "data": [{"id": "claude-opus-4-7", "object": "model", "owned_by": "anthropic"}]
    });
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .expect(1) // second call must NOT hit the server
        .mount(&server)
        .await;

    let base = format!("{}/v1", server.uri());
    let client = ModelCatalogClient::new(base, "test-key", tiers_yaml_path()).unwrap();

    let first = client.list_models().await.unwrap();
    let second = client.list_models().await.unwrap();

    assert_eq!(first.len(), 1);
    assert_eq!(second.len(), 1);
    // wiremock's `.expect(1)` fails the test on drop if the mock was hit more than once.
}
```

- [ ] **Step 4: Run the cache test and verify it passes**

```bash
cargo test -p zeroclaw-providers --test catalog_test list_models_uses_cache_on_second_call_within_ttl -- --nocapture
```
Expected: PASS.

- [ ] **Step 5: Add non-2xx failure test**

Append to `catalog_test.rs`:

```rust
#[tokio::test]
async fn list_models_surfaces_non_2xx_as_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(502).set_body_string("upstream down"))
        .mount(&server)
        .await;

    let base = format!("{}/v1", server.uri());
    let client = ModelCatalogClient::new(base, "test-key", tiers_yaml_path()).unwrap();
    let err = client.list_models().await.unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("502"), "error should mention 502: {msg}");
    assert!(msg.contains("upstream down"), "error should include body: {msg}");
}
```

- [ ] **Step 6: Run the failure test and verify it passes**

```bash
cargo test -p zeroclaw-providers --test catalog_test list_models_surfaces_non_2xx_as_error -- --nocapture
```
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/zeroclaw-providers/src/catalog.rs \
        crates/zeroclaw-providers/tests/catalog_test.rs
git commit -m "feat(providers): implement list_models with 60s cache + error propagation"
```

---

## Task 3: Implement list_tiers + resolve_tier from YAML

Tier mapping comes from a YAML file on the local filesystem (zeroclaw's volume). Re-read on each `list_tiers` call — operators can edit it without a restart, and file reads at <1/min are negligible.

**Files:**
- Modify: `crates/zeroclaw-providers/src/catalog.rs`
- Modify: `crates/zeroclaw-providers/tests/catalog_test.rs`

- [ ] **Step 1: Replace `list_tiers` stub**

In `catalog.rs`:

```rust
pub async fn list_tiers(&self) -> Result<Vec<TierEntry>> {
    let bytes = tokio::fs::read(&self.tiers_path)
        .await
        .with_context(|| format!("reading tier config at {}", self.tiers_path.display()))?;
    let parsed: TiersFile = serde_yaml::from_slice(&bytes)
        .with_context(|| format!("parsing YAML at {}", self.tiers_path.display()))?;
    Ok(parsed.tiers)
}
```

- [ ] **Step 2: Replace `resolve_tier` stub**

In `catalog.rs`:

```rust
pub async fn resolve_tier(&self, tier: &str) -> Result<String> {
    let tiers = self.list_tiers().await?;
    tiers
        .iter()
        .find(|t| t.name.eq_ignore_ascii_case(tier))
        .map(|t| t.model.clone())
        .ok_or_else(|| {
            let available: Vec<&str> = tiers.iter().map(|t| t.name.as_str()).collect();
            anyhow::anyhow!(
                "unknown tier '{tier}'. Available tiers: {}",
                available.join(", ")
            )
        })
}
```

- [ ] **Step 3: Add tier-happy-path test**

Append to `catalog_test.rs`:

```rust
use std::io::Write;

fn write_tiers_file(yaml: &str) -> tempfile::NamedTempFile {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(yaml.as_bytes()).unwrap();
    f
}

#[tokio::test]
async fn list_tiers_reads_yaml() {
    let f = write_tiers_file(
        "tiers:\n  - name: chat\n    model: claude-sonnet-4-6\n    description: default\n  - name: thinking\n    model: claude-opus-4-7\n    description: deep reasoning\n",
    );
    let server = MockServer::start().await;
    let client = ModelCatalogClient::new(
        format!("{}/v1", server.uri()),
        "test-key",
        f.path().to_path_buf(),
    )
    .unwrap();

    let tiers = client.list_tiers().await.unwrap();
    assert_eq!(tiers.len(), 2);
    assert_eq!(tiers[0].name, "chat");
    assert_eq!(tiers[1].model, "claude-opus-4-7");
}

#[tokio::test]
async fn resolve_tier_returns_model_case_insensitive() {
    let f = write_tiers_file(
        "tiers:\n  - name: thinking\n    model: claude-opus-4-7\n    description: \"\"\n",
    );
    let server = MockServer::start().await;
    let client = ModelCatalogClient::new(
        format!("{}/v1", server.uri()),
        "test-key",
        f.path().to_path_buf(),
    )
    .unwrap();

    let model = client.resolve_tier("Thinking").await.unwrap();
    assert_eq!(model, "claude-opus-4-7");
}

#[tokio::test]
async fn resolve_tier_rejects_unknown_tier() {
    let f = write_tiers_file(
        "tiers:\n  - name: chat\n    model: claude-sonnet-4-6\n    description: \"\"\n",
    );
    let server = MockServer::start().await;
    let client = ModelCatalogClient::new(
        format!("{}/v1", server.uri()),
        "test-key",
        f.path().to_path_buf(),
    )
    .unwrap();

    let err = client.resolve_tier("ultra").await.unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("unknown tier"));
    assert!(msg.contains("chat"));
}
```

- [ ] **Step 4: Add `tempfile` as a dev-dep if missing**

```bash
grep -n "tempfile" crates/zeroclaw-providers/Cargo.toml
```

If absent, add under `[dev-dependencies]`:
```toml
tempfile = "3"
```

- [ ] **Step 5: Run tier tests**

```bash
cargo test -p zeroclaw-providers --test catalog_test list_tiers_reads_yaml resolve_tier_returns_model_case_insensitive resolve_tier_rejects_unknown_tier -- --nocapture
```
Expected: all three PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/zeroclaw-providers/src/catalog.rs \
        crates/zeroclaw-providers/tests/catalog_test.rs \
        crates/zeroclaw-providers/Cargo.toml
git commit -m "feat(providers): implement list_tiers + resolve_tier from YAML"
```

---

## Task 4: Wire ModelCatalogClient into ModelSwitchTool — construction

Before adding new actions, update the tool's constructor to accept a catalog client. Use `Option<Arc<ModelCatalogClient>>` so the tool still builds when no catalog is configured (e.g. non-cliproxy setups, unit tests).

**Files:**
- Modify: `crates/zeroclaw-runtime/src/tools/model_switch.rs`
- Modify: call sites that construct `ModelSwitchTool`

- [ ] **Step 1: Find all construction sites**

Run:
```bash
grep -rn "ModelSwitchTool::new" crates/
```

Record the list — you must update each.

- [ ] **Step 2: Update `ModelSwitchTool` struct and `new`**

In `crates/zeroclaw-runtime/src/tools/model_switch.rs`, at the top of the file (around line 9), replace the struct and constructor:

```rust
use std::sync::Arc;
use zeroclaw_providers::ModelCatalogClient;

pub struct ModelSwitchTool {
    security: Arc<SecurityPolicy>,
    catalog: Option<Arc<ModelCatalogClient>>,
}

impl ModelSwitchTool {
    pub fn new(security: Arc<SecurityPolicy>) -> Self {
        Self {
            security,
            catalog: None,
        }
    }

    pub fn with_catalog(mut self, catalog: Arc<ModelCatalogClient>) -> Self {
        self.catalog = Some(catalog);
        self
    }
}
```

Remove the duplicate `use std::sync::Arc;` if it was already elsewhere — check with `grep -n "use std::sync::Arc" crates/zeroclaw-runtime/src/tools/model_switch.rs`.

- [ ] **Step 3: Build and fix compile errors**

```bash
cargo check -p zeroclaw-runtime
```

Expected: clean check (no construction site needs changes because `new` is source-compatible).

- [ ] **Step 4: Commit**

```bash
git add crates/zeroclaw-runtime/src/tools/model_switch.rs
git commit -m "refactor(runtime): ModelSwitchTool accepts optional catalog client"
```

---

## Task 5: Rewrite `list_models` to prefer the live catalog

**Files:**
- Modify: `crates/zeroclaw-runtime/src/tools/model_switch.rs`
- Create: `crates/zeroclaw-runtime/tests/model_switch_test.rs`

- [ ] **Step 1: Replace `handle_list_models`**

In `crates/zeroclaw-runtime/src/tools/model_switch.rs`, replace the existing `handle_list_models` (currently at lines 194–275) with:

```rust
async fn handle_list_models(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
    let provider = args.get("provider").and_then(|v| v.as_str()).unwrap_or("");

    // If a catalog is wired and the caller either asked for the catalog-
    // backed provider or omitted the parameter, return the live list.
    if let Some(catalog) = &self.catalog {
        if provider.is_empty() || provider.starts_with("custom:") {
            match catalog.list_models().await {
                Ok(models) => {
                    let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
                    return Ok(ToolResult {
                        success: true,
                        output: serde_json::to_string_pretty(&json!({
                            "provider": if provider.is_empty() { "catalog" } else { provider },
                            "models": ids,
                            "source": "live",
                            "count": models.len()
                        }))?,
                        error: None,
                    });
                }
                Err(e) => {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("catalog unavailable: {e:#}")),
                    });
                }
            }
        }
    }

    Ok(ToolResult {
        success: false,
        output: String::new(),
        error: Some(format!(
            "No live catalog configured for provider '{provider}'. Use action 'list_providers' or set action='list_models' without specifying a provider to query the catalog."
        )),
    })
}
```

Note the method signature changes from synchronous `fn` to `async fn` — update the `match` arm in `execute` (around line 69) from `"list_models" => self.handle_list_models(&args),` to `"list_models" => self.handle_list_models(&args).await,`.

- [ ] **Step 2: Add an async helper if sibling handlers are sync**

Check: the other `handle_*` methods are currently `fn`. Only `handle_list_models`, `handle_list_tiers`, and `handle_set_tier` need to be `async fn`. Leave the rest alone.

- [ ] **Step 3: Run providers+runtime build to catch type errors**

```bash
cargo check -p zeroclaw-runtime
```
Expected: clean.

- [ ] **Step 4: Write a tool-level integration test**

Create `crates/zeroclaw-runtime/tests/model_switch_test.rs`:

```rust
use std::sync::Arc;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};
use zeroclaw_providers::ModelCatalogClient;

// The production crate gates SecurityPolicy behind internal modules; the
// test exercises the tool via its public Tool trait impl.
use zeroclaw_api::tool::Tool;
use zeroclaw_runtime::security::SecurityPolicy;
use zeroclaw_runtime::tools::model_switch::ModelSwitchTool;

#[tokio::test]
async fn list_models_returns_live_catalog() {
    let server = MockServer::start().await;
    let body = serde_json::json!({
        "data": [
            {"id": "claude-opus-4-7",   "object": "model", "owned_by": "anthropic"},
            {"id": "claude-sonnet-4-6", "object": "model", "owned_by": "anthropic"}
        ]
    });
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;

    let catalog = Arc::new(
        ModelCatalogClient::new(
            format!("{}/v1", server.uri()),
            "test-key",
            std::path::PathBuf::from("/nonexistent"),
        )
        .unwrap(),
    );

    let tool = ModelSwitchTool::new(Arc::new(SecurityPolicy::permissive()))
        .with_catalog(catalog);

    let args = serde_json::json!({"action": "list_models"});
    let result = tool.execute(args).await.unwrap();
    assert!(result.success, "expected success, got {:?}", result.error);
    let v: serde_json::Value = serde_json::from_str(&result.output).unwrap();
    let ids: Vec<&str> = v["models"].as_array().unwrap()
        .iter().map(|x| x.as_str().unwrap()).collect();
    assert_eq!(ids, vec!["claude-opus-4-7", "claude-sonnet-4-6"]);
    assert_eq!(v["source"], "live");
}
```

- [ ] **Step 5: Verify `SecurityPolicy::permissive()` exists**

```bash
grep -n "pub fn permissive" crates/zeroclaw-runtime/src/security/
```

If a `permissive()` constructor doesn't exist, search for how other tests construct a `SecurityPolicy`:

```bash
grep -rn "SecurityPolicy::" crates/zeroclaw-runtime/src/ crates/zeroclaw-runtime/tests/ | head -10
```

Use whatever existing constructor yields an allow-all policy (likely `SecurityPolicy::default()` or `SecurityPolicy::new_for_test()`). Substitute that in the test.

If there is no public test helper, add one at the bottom of `crates/zeroclaw-runtime/src/security/policy.rs`:

```rust
#[cfg(any(test, feature = "test-helpers"))]
impl SecurityPolicy {
    /// Test-only helper: returns a policy that allows all operations.
    pub fn permissive() -> Self {
        Self::default()
    }
}
```

and gate on `#[cfg(test)]` if no `test-helpers` feature already exists. Confirm compilation:

```bash
cargo check -p zeroclaw-runtime --tests
```

- [ ] **Step 6: Run the test**

```bash
cargo test -p zeroclaw-runtime --test model_switch_test list_models_returns_live_catalog -- --nocapture
```
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/zeroclaw-runtime/src/tools/model_switch.rs \
        crates/zeroclaw-runtime/tests/model_switch_test.rs \
        crates/zeroclaw-runtime/src/security/policy.rs
git commit -m "feat(runtime): list_models serves live catalog when available"
```

---

## Task 6: Add `list_tiers` action

**Files:**
- Modify: `crates/zeroclaw-runtime/src/tools/model_switch.rs`
- Modify: `crates/zeroclaw-runtime/tests/model_switch_test.rs`

- [ ] **Step 1: Extend the tool's `parameters_schema` enum and `execute` dispatch**

In `model_switch.rs`, update the `action` enum (line ~35):

```rust
"enum": ["get", "set", "set_tier", "list_providers", "list_models", "list_tiers"],
```

Update the `execute` match (line ~65) to add:

```rust
"list_tiers"  => self.handle_list_tiers().await,
"set_tier"    => self.handle_set_tier(&args).await,
```

Also update the tool's `description` string (line 26) to mention tiers:

```rust
"Switch the AI model at runtime. Use 'list_tiers' and 'set_tier' (chat/thinking/fast) for semantic switching, or 'list_models' / 'set' for raw model IDs. 'get' shows current model."
```

- [ ] **Step 2: Implement `handle_list_tiers`**

Append to the `impl ModelSwitchTool { ... }` block:

```rust
async fn handle_list_tiers(&self) -> anyhow::Result<ToolResult> {
    let Some(catalog) = &self.catalog else {
        return Ok(ToolResult {
            success: false,
            output: String::new(),
            error: Some("tier catalog not configured for this deployment".to_string()),
        });
    };

    match catalog.list_tiers().await {
        Ok(tiers) => Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&json!({
                "tiers": tiers.iter().map(|t| json!({
                    "name": t.name,
                    "model": t.model,
                    "description": t.description,
                })).collect::<Vec<_>>(),
                "usage": "Call set_tier with {\"tier\": \"chat\" | \"thinking\" | \"fast\"}"
            }))?,
            error: None,
        }),
        Err(e) => Ok(ToolResult {
            success: false,
            output: String::new(),
            error: Some(format!("failed to load tiers: {e:#}")),
        }),
    }
}
```

- [ ] **Step 3: Add integration test**

Append to `model_switch_test.rs`:

```rust
use std::io::Write;

fn write_tiers(yaml: &str) -> tempfile::NamedTempFile {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(yaml.as_bytes()).unwrap();
    f
}

#[tokio::test]
async fn list_tiers_returns_yaml_contents() {
    let server = MockServer::start().await;
    let f = write_tiers(
        "tiers:\n  - name: chat\n    model: claude-sonnet-4-6\n    description: default\n  - name: thinking\n    model: claude-opus-4-7\n    description: deep reasoning\n",
    );

    let catalog = Arc::new(
        ModelCatalogClient::new(
            format!("{}/v1", server.uri()),
            "test-key",
            f.path().to_path_buf(),
        )
        .unwrap(),
    );
    let tool = ModelSwitchTool::new(Arc::new(SecurityPolicy::permissive()))
        .with_catalog(catalog);

    let result = tool.execute(serde_json::json!({"action": "list_tiers"})).await.unwrap();
    assert!(result.success, "error: {:?}", result.error);
    let v: serde_json::Value = serde_json::from_str(&result.output).unwrap();
    let names: Vec<&str> = v["tiers"].as_array().unwrap()
        .iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, vec!["chat", "thinking"]);
}
```

Ensure `tempfile = "3"` is in `crates/zeroclaw-runtime/Cargo.toml` under `[dev-dependencies]`; if missing, add it.

- [ ] **Step 4: Run the test**

```bash
cargo test -p zeroclaw-runtime --test model_switch_test list_tiers_returns_yaml_contents -- --nocapture
```
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/zeroclaw-runtime/src/tools/model_switch.rs \
        crates/zeroclaw-runtime/tests/model_switch_test.rs \
        crates/zeroclaw-runtime/Cargo.toml
git commit -m "feat(runtime): add list_tiers action to model_switch"
```

---

## Task 7: Add `set_tier` action + validate `set` against live catalog

**Files:**
- Modify: `crates/zeroclaw-runtime/src/tools/model_switch.rs`
- Modify: `crates/zeroclaw-runtime/tests/model_switch_test.rs`

- [ ] **Step 1: Implement `handle_set_tier`**

Append to the impl block:

```rust
async fn handle_set_tier(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
    let Some(catalog) = &self.catalog else {
        return Ok(ToolResult {
            success: false,
            output: String::new(),
            error: Some("tier catalog not configured for this deployment".to_string()),
        });
    };

    let Some(tier) = args.get("tier").and_then(|v| v.as_str()) else {
        return Ok(ToolResult {
            success: false,
            output: String::new(),
            error: Some("Missing 'tier' parameter. Use 'list_tiers' to see options.".to_string()),
        });
    };

    let model = match catalog.resolve_tier(tier).await {
        Ok(m) => m,
        Err(e) => {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("{e:#}")),
            });
        }
    };

    // Use the base_url-prefixed custom provider key so the switch resolves
    // to the same provider the agent is already talking to.
    let provider = catalog.provider_key();
    let switch_state = get_model_switch_state();
    *switch_state.lock().unwrap() = Some((provider.clone(), model.clone()));

    Ok(ToolResult {
        success: true,
        output: serde_json::to_string_pretty(&json!({
            "tier": tier,
            "resolved_model": model,
            "provider": provider,
            "note": "Switch takes effect on next agent turn."
        }))?,
        error: None,
    })
}
```

- [ ] **Step 2: Add `provider_key` accessor to `ModelCatalogClient`**

In `catalog.rs`, add inside `impl ModelCatalogClient`:

```rust
/// Returns the provider key (e.g. `custom:http://adi-cliproxy.internal:8317/v1`)
/// that callers should use when staging a model switch that targets this
/// catalog's provider.
pub fn provider_key(&self) -> String {
    // base_url is expected to be "<scheme>://host[:port]/v1".
    // Strip the trailing "/v1" to produce the provider key, then prefix.
    let trimmed = self
        .base_url
        .trim_end_matches('/')
        .trim_end_matches("/v1");
    format!("custom:{trimmed}")
}
```

- [ ] **Step 3: Add `set_tier` test**

Append to `model_switch_test.rs`:

```rust
#[tokio::test]
async fn set_tier_stages_resolved_model() {
    let server = MockServer::start().await;
    let f = write_tiers(
        "tiers:\n  - name: thinking\n    model: claude-opus-4-7\n    description: deep\n",
    );

    let catalog = Arc::new(
        ModelCatalogClient::new(
            format!("{}/v1", server.uri()),
            "test-key",
            f.path().to_path_buf(),
        )
        .unwrap(),
    );
    let tool = ModelSwitchTool::new(Arc::new(SecurityPolicy::permissive()))
        .with_catalog(catalog);

    let result = tool
        .execute(serde_json::json!({"action": "set_tier", "tier": "thinking"}))
        .await
        .unwrap();
    assert!(result.success, "error: {:?}", result.error);

    let v: serde_json::Value = serde_json::from_str(&result.output).unwrap();
    assert_eq!(v["tier"], "thinking");
    assert_eq!(v["resolved_model"], "claude-opus-4-7");
    assert!(v["provider"].as_str().unwrap().starts_with("custom:"));
}

#[tokio::test]
async fn set_tier_rejects_unknown_tier() {
    let server = MockServer::start().await;
    let f = write_tiers(
        "tiers:\n  - name: chat\n    model: claude-sonnet-4-6\n    description: \"\"\n",
    );
    let catalog = Arc::new(
        ModelCatalogClient::new(
            format!("{}/v1", server.uri()),
            "test-key",
            f.path().to_path_buf(),
        )
        .unwrap(),
    );
    let tool = ModelSwitchTool::new(Arc::new(SecurityPolicy::permissive()))
        .with_catalog(catalog);

    let result = tool
        .execute(serde_json::json!({"action": "set_tier", "tier": "ultra"}))
        .await
        .unwrap();
    assert!(!result.success);
    assert!(result.error.unwrap().contains("unknown tier"));
}
```

- [ ] **Step 4: Run the tests**

```bash
cargo test -p zeroclaw-runtime --test model_switch_test set_tier_stages_resolved_model set_tier_rejects_unknown_tier -- --nocapture
```
Expected: both PASS.

- [ ] **Step 5: Validate raw `set` against the live catalog**

In `model_switch.rs`, change `fn handle_set` to `async fn handle_set`, update the dispatch match arm `"set" => self.handle_set(&args).await,`, and inject a validation step for catalog-backed providers. Insert immediately before the `// Set the global model switch request` comment (around line 152):

```rust
if let Some(catalog) = &self.catalog {
    if provider.starts_with("custom:") {
        match catalog.list_models().await {
            Ok(models) => {
                let known = models.iter().any(|m| m.id == model);
                if !known {
                    let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
                    return Ok(ToolResult {
                        success: false,
                        output: serde_json::to_string_pretty(&json!({
                            "available_models": ids
                        }))?,
                        error: Some(format!(
                            "Unknown model '{model}' for provider '{provider}'. See available_models."
                        )),
                    });
                }
            }
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("cannot validate model: catalog unreachable: {e:#}")),
                });
            }
        }
    }
}
```

- [ ] **Step 6: Add a `set`-validation test**

Append to `model_switch_test.rs`:

```rust
#[tokio::test]
async fn set_rejects_unknown_model_against_live_catalog() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [{"id": "claude-sonnet-4-6", "object": "model", "owned_by": "anthropic"}]
        })))
        .mount(&server)
        .await;

    let f = write_tiers("tiers: []\n");
    let catalog = Arc::new(
        ModelCatalogClient::new(
            format!("{}/v1", server.uri()),
            "test-key",
            f.path().to_path_buf(),
        )
        .unwrap(),
    );
    let tool = ModelSwitchTool::new(Arc::new(SecurityPolicy::permissive()))
        .with_catalog(catalog);

    let result = tool
        .execute(serde_json::json!({
            "action": "set",
            "provider": format!("custom:{}", server.uri()),
            "model": "claude-sonnet-4-5"  // the stale name that broke prod
        }))
        .await
        .unwrap();
    assert!(!result.success);
    let err = result.error.unwrap();
    assert!(err.contains("Unknown model"), "unexpected error: {err}");
}
```

- [ ] **Step 7: Run the set-validation test**

```bash
cargo test -p zeroclaw-runtime --test model_switch_test set_rejects_unknown_model_against_live_catalog -- --nocapture
```
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/zeroclaw-providers/src/catalog.rs \
        crates/zeroclaw-runtime/src/tools/model_switch.rs \
        crates/zeroclaw-runtime/tests/model_switch_test.rs
git commit -m "feat(runtime): set_tier + validate set against live catalog"
```

---

## Task 8: Drop the hardcoded model list

The dead code must go — leaving it is a trap.

**Files:**
- Modify: `crates/zeroclaw-runtime/src/tools/model_switch.rs`

- [ ] **Step 1: Remove the static `match provider.to_lowercase().as_str()` block**

In `handle_list_models` (now the catalog-backed version), the stale match block lists `"openai" => vec![...]`, `"anthropic" => vec![...]`, etc. Delete it entirely along with the "No common models listed…" fallback. After the catalog check, the function should fall through to returning a "no live catalog" error as already written in Task 5 Step 1. Double-check by reading the function top-to-bottom.

- [ ] **Step 2: Build + run the full runtime test suite**

```bash
cargo test -p zeroclaw-runtime
```
Expected: all tests PASS, zero warnings about unused constants.

- [ ] **Step 3: Commit**

```bash
git add crates/zeroclaw-runtime/src/tools/model_switch.rs
git commit -m "chore(runtime): remove hardcoded model list from model_switch"
```

---

## Task 9: Silent-hang fix — surface non-2xx at the chat endpoint

The 2026-04-23 incident's root cause. `compatible.rs` returns a `reqwest::Response`; its body gets streamed to the agent loop without an upfront status check, and a 502 on the streaming path is easy to lose. Fix: after `.send()`, check status; on non-2xx, read the body (once), log, and return a typed error.

**Files:**
- Modify: `crates/zeroclaw-providers/src/compatible.rs`
- Create/modify: `crates/zeroclaw-providers/tests/compatible_http_test.rs`

- [ ] **Step 1: Locate the chat-endpoint request site**

```bash
grep -n "chat/completions\|\.send()\.await" crates/zeroclaw-providers/src/compatible.rs | head -20
```

Record the main chat-completions call site(s). There are likely two: non-streaming and streaming.

- [ ] **Step 2: Wrap the response with an explicit status check (non-streaming path)**

Find the block that does `let resp = self.http.post(&url).json(&body).send().await?;` for the non-streaming path. Immediately after `.await?` and before any `.json()` / `.bytes_stream()`, insert:

```rust
let status = resp.status();
if !status.is_success() {
    let body_text = resp.text().await.unwrap_or_default();
    let truncated: String = body_text.chars().take(MAX_API_ERROR_CHARS).collect();
    tracing::warn!(
        provider = %self.name,
        url = %url,
        status = %status,
        model = %request.model,
        body = %truncated,
        "LLM provider returned non-2xx"
    );
    anyhow::bail!(
        "provider '{}' returned HTTP {} for model '{}': {}",
        self.name, status, request.model, truncated
    );
}
```

Use the existing `MAX_API_ERROR_CHARS` constant (defined in `lib.rs` at line 52; re-export it from the crate root if not already accessible, or re-declare locally in `compatible.rs` — do not duplicate the magic number). Verify:

```bash
grep -n "MAX_API_ERROR_CHARS" crates/zeroclaw-providers/src/
```

If it's not accessible, add `pub(crate) const MAX_API_ERROR_CHARS: usize = 500;` at the top of `compatible.rs` and reference `crate::compatible::MAX_API_ERROR_CHARS` from here.

- [ ] **Step 3: Apply the same pattern to the streaming path**

Find the streaming call site (builds a `bytes_stream()` from the response). Before handing the response to `sse_bytes_to_chunks`, do the same status check. On non-2xx, read the body and return an error rather than streaming.

- [ ] **Step 4: Add a regression test**

Create `crates/zeroclaw-providers/tests/compatible_http_test.rs`:

```rust
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};
use zeroclaw_providers::{compatible::{OpenAiCompatibleProvider, AuthStyle}, traits::{ChatMessage, ChatRequest, Provider}};

#[tokio::test]
async fn non_2xx_surfaces_structured_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(502).set_body_json(serde_json::json!({
            "error": {"message": "unknown provider for model anthropic/claude-sonnet-4-5"}
        })))
        .mount(&server)
        .await;

    let provider = OpenAiCompatibleProvider::new_with_vision(
        "Test",
        &server.uri(),
        "test-key",
        AuthStyle::Bearer,
        true,
    );
    let req = ChatRequest {
        model: "anthropic/claude-sonnet-4-5".to_string(),
        messages: vec![ChatMessage::user("hi")],
        ..ChatRequest::default()
    };

    let start = std::time::Instant::now();
    let err = provider.chat(req).await.unwrap_err();
    let elapsed = start.elapsed();
    assert!(elapsed < std::time::Duration::from_secs(5),
        "should fail fast, not hang (took {elapsed:?})");
    let msg = format!("{err:#}");
    assert!(msg.contains("502"), "error should contain status: {msg}");
    assert!(msg.contains("unknown provider for model"),
        "error should include upstream body: {msg}");
}
```

Note: the public API of `OpenAiCompatibleProvider::new_with_vision` and `ChatMessage::user` may not match exactly. Run `cargo check --test compatible_http_test -p zeroclaw-providers` and adjust the test to the actual signatures found in `traits.rs` and `compatible.rs`. Do NOT change the production signatures — only the test calls.

- [ ] **Step 5: Run the regression test**

```bash
cargo test -p zeroclaw-providers --test compatible_http_test -- --nocapture
```
Expected: PASS. Must fail in under 5 seconds even if provider-level retries are configured. If this assertion fails with a timeout in seconds, the retry logic is the culprit — reduce retries to zero for the test via `OpenAiCompatibleProvider::with_retries(0)` or whatever builder is available.

- [ ] **Step 6: Commit**

```bash
git add crates/zeroclaw-providers/src/compatible.rs \
        crates/zeroclaw-providers/tests/compatible_http_test.rs
git commit -m "fix(providers): surface non-2xx responses as errors instead of hanging"
```

---

## Task 10: Wire the catalog into ModelSwitchTool at daemon startup

Up to here the catalog is plumbed through construction but no daemon actually passes one. Fix that now.

**Files:**
- Modify: the module that constructs `ModelSwitchTool` at daemon startup

- [ ] **Step 1: Locate the construction call site from Task 4's grep output**

```bash
grep -rn "ModelSwitchTool::new" crates/
```

Typical location is under `crates/zeroclaw-runtime/src/tools/mod.rs` or `crates/zeroclaw-runtime/src/channels/` wiring. Open the file that assembles the tool set at daemon init.

- [ ] **Step 2: Build a `ModelCatalogClient` from the active provider config**

The daemon already reads `[providers.models."custom:…"]` blocks. Find the code that selects the fallback provider URL and API key (the `provider.fallback` in `config.toml` resolves to the `custom:http://adi-cliproxy.internal:8317/v1` string). At the construction point for `ModelSwitchTool`, do:

```rust
use std::sync::Arc;
use zeroclaw_providers::ModelCatalogClient;

let catalog = if let Some(fallback_url) = fallback_provider_url_if_custom(&config) {
    let api_key = resolve_provider_api_key(&config, &fallback_url);
    let tiers_path = std::path::PathBuf::from("/zeroclaw-data/.zeroclaw/tiers.yaml");
    // Outside of Fly (dev), fall back to XDG path or skip.
    let tiers_path = if tiers_path.exists() {
        tiers_path
    } else {
        directories::ProjectDirs::from("", "", "zeroclaw")
            .map(|d| d.config_dir().join("tiers.yaml"))
            .unwrap_or_else(|| std::path::PathBuf::from("tiers.yaml"))
    };
    match ModelCatalogClient::new(fallback_url.trim_start_matches("custom:"), api_key, tiers_path) {
        Ok(c) => Some(Arc::new(c)),
        Err(e) => {
            tracing::warn!(error = %e, "failed to build model catalog client; tiers disabled");
            None
        }
    }
} else {
    None
};

let mut tool = ModelSwitchTool::new(security.clone());
if let Some(c) = catalog {
    tool = tool.with_catalog(c);
}
```

`fallback_provider_url_if_custom` and `resolve_provider_api_key` are spot-fixtures — adapt to whatever helper already exists in the config layer. Search:

```bash
grep -rn "providers.models\|fallback" crates/zeroclaw-config/src/ | head -20
```

Use the real helpers; do not invent a new config loader.

- [ ] **Step 3: Build + smoke-run**

```bash
./dev/ci.sh all
```
Expected: clean build, all tests PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/zeroclaw-runtime/src/
git commit -m "feat(runtime): wire ModelCatalogClient into daemon ModelSwitchTool"
```

---

## Task 11: Seed `tiers.yaml` in the Docker image

**Files:**
- Create: `deploy/zeroclaw/tiers.seed.yaml`
- Modify: `deploy/zeroclaw/Dockerfile`
- Modify: `deploy/zeroclaw/entrypoint.sh`

- [ ] **Step 1: Create the seed file**

Create `deploy/zeroclaw/tiers.seed.yaml`:

```yaml
# Seeded to /zeroclaw-data/.zeroclaw/tiers.yaml on first boot.
#
# After seeding, edit the live file and restart the machine to apply.
# Model names must match cliproxy's /v1/models catalog; bad names will be
# rejected by the model_switch tool with a list of valid options.
tiers:
  - name: chat
    model: claude-sonnet-4-6
    description: Default conversational model. Channel replies, routine tool use.
  - name: thinking
    model: claude-opus-4-7
    description: Deep reasoning, planning, code review, multi-step analysis.
  - name: fast
    model: claude-haiku-4-5-20251001
    description: Cheap classification, triage, short summarization.
```

- [ ] **Step 2: COPY it into the image**

In `deploy/zeroclaw/Dockerfile`, find the existing `COPY deploy/zeroclaw/config.seed.toml /opt/zeroclaw.config.seed.toml` line (around line 69). Add immediately below:

```dockerfile
COPY deploy/zeroclaw/tiers.seed.yaml /opt/zeroclaw.tiers.seed.yaml
```

- [ ] **Step 3: Seed on first boot**

In `deploy/zeroclaw/entrypoint.sh`, find the existing "Seed config" block (around line 110). Add a parallel block immediately after it:

```bash
# ── Seed tiers.yaml ────────────────────────────────────────────────
ZEROCLAW_TIERS="/zeroclaw-data/.zeroclaw/tiers.yaml"
ZEROCLAW_TIERS_SEED="/opt/zeroclaw.tiers.seed.yaml"
if [[ ! -f "$ZEROCLAW_TIERS" ]]; then
  log "seeding $ZEROCLAW_TIERS from $ZEROCLAW_TIERS_SEED (first boot)"
  cp "$ZEROCLAW_TIERS_SEED" "$ZEROCLAW_TIERS"
  chmod 644 "$ZEROCLAW_TIERS"
else
  log "existing $ZEROCLAW_TIERS preserved (edit-and-restart to update)"
fi
```

- [ ] **Step 4: Validate the Dockerfile still builds (local syntax check)**

```bash
docker buildx build --check -f deploy/zeroclaw/Dockerfile deploy/zeroclaw/ 2>&1 | tail -5
```

Expected: `no warnings`. (Full image build is deferred to the deploy step — this only catches syntax.)

- [ ] **Step 5: Commit**

```bash
git add deploy/zeroclaw/tiers.seed.yaml \
        deploy/zeroclaw/Dockerfile \
        deploy/zeroclaw/entrypoint.sh
git commit -m "deploy(zeroclaw): seed /zeroclaw-data/.zeroclaw/tiers.yaml on first boot"
```

---

## Task 12: Documentation

**Files:**
- Create: `docs/reference/api/model-tiers.md`

- [ ] **Step 1: Create the reference doc**

Create `docs/reference/api/model-tiers.md`:

```markdown
# Model tiers and live catalog

Zeroclaw's `model_switch` tool resolves semantic tiers (`chat`, `thinking`, `fast`) to concrete model IDs via a local YAML file and validates raw model IDs against the provider's live `/v1/models` catalog.

## Agent-facing actions

- `list_tiers` — returns the configured tiers with their resolved model IDs and descriptions.
- `set_tier { tier: "chat" | "thinking" | "fast" }` — stages a tier switch applied on the next agent turn.
- `list_models` — returns the live catalog from the provider's `/v1/models` (cached 60s per process).
- `set { provider, model }` — validates the model against the live catalog before staging. Unknown names are rejected with the available list.

## Configuration

Tier mapping lives in `/zeroclaw-data/.zeroclaw/tiers.yaml` (seeded by the Docker image on first boot). Example:

```yaml
tiers:
  - name: chat
    model: claude-sonnet-4-6
    description: Default.
  - name: thinking
    model: claude-opus-4-7
    description: Deep reasoning.
  - name: fast
    model: claude-haiku-4-5-20251001
    description: Classification / triage.
```

After editing, `flyctl machine restart <id> -a <app>` to reload. The file is re-read on each `list_tiers` call, so restart is only needed for effect on an in-flight conversation.

## Failure modes

- Catalog unreachable → `list_models` and raw `set` return a structured error mentioning HTTP status + body.
- Unknown tier → `set_tier` returns "unknown tier 'X'. Available tiers: chat, thinking, fast".
- Unknown model ID → `set` returns "Unknown model 'X'. See available_models."

## Why tiers

- The agent thinks in capability terms (`thinking` vs `chat`) rather than memorizing dated model IDs that change as the provider ships new models.
- Operators promote a new model (e.g. `claude-opus-4-8`) by editing one line in `tiers.yaml` and restarting — no zeroclaw rebuild.
```

- [ ] **Step 2: Commit**

```bash
git add docs/reference/api/model-tiers.md
git commit -m "docs: reference for model tiers + live catalog"
```

---

## Task 13: Full CI + deploy dry-run

- [ ] **Step 1: Run the full test suite**

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Expected: all green. Fix any issues before proceeding.

- [ ] **Step 2: Run the project's canonical pre-PR gate**

```bash
./dev/ci.sh all
```

Expected: PASS.

- [ ] **Step 3: Deploy shane (canary)**

```bash
flyctl deploy \
  --config deploy/zeroclaw/fly.shane.toml \
  --dockerfile deploy/zeroclaw/Dockerfile \
  --build-secret "adi_persona_token=$(gh auth token)"
```

Watch for the line `image size:` — should be ~1–5 MB larger than the previous deploy (accounting for the added `tiers.seed.yaml` and Rust binary growth from the catalog client).

- [ ] **Step 4: Verify tiers.yaml was seeded**

```bash
SHANE_MID=$(flyctl machines list -a adi-zeroclaw-shane | grep -E '^\s*[a-f0-9]{14}' | head -1 | awk '{print $1}')
flyctl machine exec "$SHANE_MID" 'cat /zeroclaw-data/.zeroclaw/tiers.yaml' -a adi-zeroclaw-shane
```

Expected: prints the three-tier YAML.

- [ ] **Step 5: Smoke-test the tool via the gateway**

Send a telegram message to shane: "List your available tiers." The reply should enumerate `chat`, `thinking`, `fast` with descriptions.

Then send: "Switch to thinking and explain one thing you'd do differently with more reasoning budget." The reply should be markedly more considered; check `flyctl logs -a adi-zeroclaw-shane --no-tail | grep -E "tier|Opus|model"` for confirmation of the switch.

- [ ] **Step 6: Deploy meg**

```bash
flyctl deploy \
  --config deploy/zeroclaw/fly.meg.toml \
  --dockerfile deploy/zeroclaw/Dockerfile \
  --build-secret "adi_persona_token=$(gh auth token)"
```

- [ ] **Step 7: Verify tiers on meg**

```bash
MEG_MID=$(flyctl machines list -a adi-zeroclaw-meg | grep -E '^\s*[a-f0-9]{14}' | head -1 | awk '{print $1}')
flyctl machine exec "$MEG_MID" 'cat /zeroclaw-data/.zeroclaw/tiers.yaml' -a adi-zeroclaw-meg
```

Expected: identical tier config to shane (since both seed from the image).

- [ ] **Step 8: Open PR to master**

```bash
git push -u origin feature/model-tiers
gh pr create --base master --title "feat: live model discovery + tier-based switching" --body "$(cat <<'EOF'
## Summary

- Live `/v1/models` catalog fetched from cliproxy (60s cache), replacing the hardcoded model list in `model_switch.rs`.
- New tool actions `list_tiers` / `set_tier` (chat / thinking / fast), agent-driven switch on Opus for reasoning tasks.
- Silent-hang fix: non-2xx from cliproxy now surfaces as a structured error with status + body instead of hanging the agent loop.
- Tier mapping lives in `/zeroclaw-data/.zeroclaw/tiers.yaml`, seeded by the image on first boot.

## Test plan

- [x] `cargo test` green
- [x] `./dev/ci.sh all` green
- [x] Deployed to shane, smoke-tested tier switching via telegram
- [x] Deployed to meg, confirmed tier seed
- [x] Confirmed non-2xx no longer hangs (regression test in `compatible_http_test.rs`)

## Spec

See `docs/superpowers/specs/2026-04-23-model-tiers-design.md`.
EOF
)"
```

---

## Self-review (performed by plan author)

**Spec coverage:**
- Live `/v1/models` via `ModelCatalogClient` → Tasks 1-2 ✓
- `list_tiers` + `set_tier` actions → Tasks 3, 6, 7 ✓
- `list_models` rewired to catalog → Task 5 ✓
- `set` validates against catalog → Task 7 ✓
- Delete hardcoded list → Task 8 ✓
- Silent-hang fix (non-2xx + timeouts) → Task 9 ✓
- Tier mapping source of truth → Tasks 3, 11 ✓ (deviation from spec: per-zeroclaw volume instead of cliproxy; documented in header)
- Agent-facing tool surface (table in spec) → Tasks 5, 6, 7 ✓
- System prompt tier doc → **gap.** Spec mentions updating persona system prompts; this plan does not modify the adi-persona repo. **Action: added to rollout discussion below.**
- 60s cache TTL → Task 2 ✓
- Tests: unit + integration + silent-hang regression → Tasks 2, 3, 5-7, 9 ✓

**Placeholder scan:** No `TBD`, no "implement appropriate error handling", no "similar to earlier task" without code. Task 9 steps 2-3 and Task 10 step 2 reference helpers the engineer must find via `grep` and adapt — that's appropriate because the exact signatures live in code the plan hasn't pinned down; the steps include the grep commands to find them. Acceptable.

**Type consistency:** `ModelEntry.id`, `TierEntry.{name,model,description}`, `ModelCatalogClient::{new,list_models,list_tiers,resolve_tier,provider_key}` all used consistently across Tasks 1–7.

**Open follow-up not in this plan:** persona system-prompt update (adi-persona repo) to mention tier switching in Adi's instructions. That repo is separate from this zeroclaw change; queue as a follow-up PR after this one lands. Not adding as a task because it requires credentials for a different repo and the feature works without it (the agent discovers `list_tiers` via tool schema).

---

## Execution handoff

Plan complete and saved to `docs/superpowers/plans/2026-04-23-model-tiers.md`. Two execution options:

**1. Subagent-Driven (recommended)** — I dispatch a fresh subagent per task, review between tasks, fast iteration.

**2. Inline Execution** — Execute tasks in this session using executing-plans, batch execution with checkpoints.

Which approach?
